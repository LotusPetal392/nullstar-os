---- MODULE JobLifecycle ----
EXTENDS FiniteSets, Naturals, Sequences

\* NullStar job lifecycle safety model.
\*
\* JobContainment.tla already checks hierarchy construction, non-relaxable
\* membership, fork inheritance, and tightening-only admission policy. This
\* refinement starts from a fixed root/child hierarchy and models the lifecycle
\* rules layered on top of that containment:
\*
\* - bounded membership plus lossless completion retention;
\* - one kernel lifetime root for each active member;
\* - snapshot-based subtree termination requests;
\* - subtree completion drainage with per-job FIFO ordering;
\* - empty-leaf retirement and post-retirement reclamation;
\* - final userspace handle closure without implicit kill-on-close.
\*
\* Scheduler delivery, signal-9 mechanics, status payloads, process-ID reuse,
\* generic garbage collection of abandoned non-retired roots, and service-manager
\* retry policy are deliberately outside this model.

CONSTANTS
    P0, P1, P2,
    J0, J1,
    NoJob,
    MaxCapacity

Processes == {P0, P1, P2}
Jobs == {J0, J1}

ASSUME /\ Cardinality(Processes) = 3
       /\ Cardinality(Jobs) = 2
       /\ NoJob \notin Jobs
       /\ MaxCapacity \in Nat
       /\ MaxCapacity >= 1

VARIABLES
    objectPresent,
    retired,
    childAttached,
    handles,
    members,
    completions,
    processJob,
    seen,
    exited,
    drained,
    terminationRequested,
    rootCount

vars == <<
    objectPresent,
    retired,
    childAttached,
    handles,
    members,
    completions,
    processJob,
    seen,
    exited,
    drained,
    terminationRequested,
    rootCount
>>

SeqSet(sequence) ==
    {sequence[index] : index \in 1..Len(sequence)}

AllPending ==
    UNION {SeqSet(completions[job]) : job \in Jobs}

Occupancy(job) ==
    Cardinality(members[job]) + Len(completions[job])

Subtree(job) ==
    IF job = J0 /\ childAttached
    THEN {J0, J1}
    ELSE {job}

SubtreeMembers(job) ==
    UNION {members[descendant] : descendant \in Subtree(job)}

SubtreeReadable(job) ==
    \E descendant \in Subtree(job): Len(completions[descendant]) > 0

SubtreeTerminated(job) ==
    SubtreeMembers(job) = {}

Init ==
    /\ objectPresent = Jobs
    /\ retired = {}
    /\ childAttached = TRUE
    /\ handles = [job \in Jobs |-> 1]
    /\ members = [job \in Jobs |-> {}]
    /\ completions = [job \in Jobs |-> <<>>]
    /\ processJob = [process \in Processes |-> NoJob]
    /\ seen = {}
    /\ exited = {}
    /\ drained = {}
    /\ terminationRequested = {}
    /\ rootCount = [job \in Jobs |-> 0]

\* Admission here abstracts either direct assignment or inherited fork after the
\* containment layer has already authorized the target job. Undrained completion
\* records consume the same bound as active members, matching kernel::job::State.
Admit ==
    \E job \in objectPresent \ retired:
        \E process \in Processes \ seen:
            /\ handles[job] = 1
            /\ Occupancy(job) < MaxCapacity
            /\ members' = [members EXCEPT ![job] = @ \cup {process}]
            /\ processJob' = [processJob EXCEPT ![process] = job]
            /\ seen' = seen \cup {process}
            /\ rootCount' = [rootCount EXCEPT ![job] = @ + 1]
            /\ UNCHANGED <<
                   objectPresent, retired, childAttached, handles, completions,
                   exited, drained, terminationRequested
               >>

\* Process completion atomically moves one active member into that job's retained
\* FIFO completion queue. The abstract transition removes the corresponding
\* kernel lifetime root only after the completion exists.
Exit ==
    \E job \in objectPresent \ retired:
        \E process \in members[job]:
            /\ members' = [members EXCEPT ![job] = @ \ {process}]
            /\ completions' = [completions EXCEPT
                   ![job] = Append(@, process)]
            /\ exited' = exited \cup {process}
            /\ rootCount' = [rootCount EXCEPT ![job] = @ - 1]
            /\ UNCHANGED <<
                   objectPresent, retired, childAttached, handles, processJob,
                   seen, drained, terminationRequested
               >>

\* JOB_TERMINATE is intentionally a bounded snapshot operation rather than a
\* sticky terminating state. It requests signal delivery for the members present
\* in the selected subtree at this transition; later admissions are independent.
TerminateSnapshot ==
    \E job \in objectPresent \ retired:
        /\ handles[job] = 1
        /\ terminationRequested' =
               terminationRequested \cup SubtreeMembers(job)
        /\ UNCHANGED <<
               objectPresent, retired, childAttached, handles, members,
               completions, processJob, seen, exited, drained, rootCount
           >>

\* A WAIT-authorized subtree drain may consume a completion from any descendant.
\* Selection between different descendant jobs is abstracted, but each individual
\* job queue remains FIFO because only Head/Tail are used.
DrainSubtree ==
    \E observer \in objectPresent \ retired:
        /\ handles[observer] = 1
        /\ \E source \in Subtree(observer):
            /\ Len(completions[source]) > 0
            /\ LET process == Head(completions[source])
               IN /\ completions' = [completions EXCEPT
                            ![source] = Tail(@)]
                  /\ drained' = drained \cup {process}
        /\ UNCHANGED <<
               objectPresent, retired, childAttached, handles, members,
               processJob, seen, exited, terminationRequested, rootCount
           >>

\* Closing the final modeled userspace handle never changes membership, queued
\* completion records, or kernel roots. In particular, close is not kill-on-close.
CloseFinalHandle ==
    \E job \in objectPresent:
        /\ handles[job] = 1
        /\ handles' = [handles EXCEPT ![job] = 0]
        /\ UNCHANGED <<
               objectPresent, retired, childAttached, members, completions,
               processJob, seen, exited, drained, terminationRequested, rootCount
           >>

\* Only the empty child leaf can retire in this fixed two-job refinement. Pending
\* completions must be drained first. Retirement detaches the hierarchy edge and
\* permanently makes the child inert; the object may still exist while a handle
\* references it.
RetireChild ==
    /\ J1 \in objectPresent
    /\ J1 \notin retired
    /\ childAttached
    /\ handles[J1] = 1
    /\ members[J1] = {}
    /\ Len(completions[J1]) = 0
    /\ retired' = retired \cup {J1}
    /\ childAttached' = FALSE
    /\ UNCHANGED <<
           objectPresent, handles, members, completions, processJob, seen,
           exited, drained, terminationRequested, rootCount
       >>

\* After retirement has detached the child and the final userspace handle is
\* gone, reclamation cannot discard members or completion records because both
\* were required empty by RetireChild and no later admission is permitted.
ReclaimRetiredChild ==
    /\ J1 \in objectPresent
    /\ J1 \in retired
    /\ ~childAttached
    /\ handles[J1] = 0
    /\ members[J1] = {}
    /\ Len(completions[J1]) = 0
    /\ objectPresent' = objectPresent \ {J1}
    /\ UNCHANGED <<
           retired, childAttached, handles, members, completions, processJob,
           seen, exited, drained, terminationRequested, rootCount
       >>

Next ==
    \/ Admit
    \/ Exit
    \/ TerminateSnapshot
    \/ DrainSubtree
    \/ CloseFinalHandle
    \/ RetireChild
    \/ ReclaimRetiredChild

Spec == Init /\ [][Next]_vars

TypeOK ==
    /\ objectPresent \subseteq Jobs
    /\ retired \subseteq Jobs
    /\ childAttached \in BOOLEAN
    /\ handles \in [Jobs -> 0..1]
    /\ members \in [Jobs -> SUBSET Processes]
    /\ completions \in [Jobs -> Seq(Processes)]
    /\ processJob \in [Processes -> Jobs \cup {NoJob}]
    /\ seen \subseteq Processes
    /\ exited \subseteq Processes
    /\ drained \subseteq Processes
    /\ terminationRequested \subseteq Processes
    /\ rootCount \in [Jobs -> 0..Cardinality(Processes)]
    /\ \A job \in Jobs \ objectPresent:
           /\ handles[job] = 0
           /\ members[job] = {}
           /\ Len(completions[job]) = 0
           /\ rootCount[job] = 0
    /\ \A process \in Processes \ seen:
           processJob[process] = NoJob

MembershipIsUnique ==
    /\ members[J0] \cap members[J1] = {}
    /\ \A process \in seen:
           processJob[process] \in Jobs
    /\ \A job \in Jobs:
           members[job] =
               {process \in seen:
                   processJob[process] = job /\ process \notin exited}

KernelRootsTrackMembers ==
    \A job \in Jobs:
        rootCount[job] = Cardinality(members[job])

ActiveMembersKeepJobsAlive ==
    \A job \in Jobs:
        members[job] # {} =>
            /\ job \in objectPresent
            /\ rootCount[job] > 0

CompletionQueuesAreUnique ==
    \A job \in Jobs:
        Cardinality(SeqSet(completions[job])) = Len(completions[job])

CompletionAccountingIsExact ==
    /\ exited \subseteq seen
    /\ drained \subseteq exited
    /\ AllPending = exited \ drained
    /\ \A job \in Jobs:
           SeqSet(completions[job]) =
               {process \in exited \ drained:
                   processJob[process] = job}

BoundedRetention ==
    \A job \in Jobs:
        Occupancy(job) <= MaxCapacity

TerminationHasProcessProvenance ==
    terminationRequested \subseteq seen

FinalHandleCloseDoesNotReleaseMembers ==
    \A job \in Jobs:
        handles[job] = 0 /\ members[job] # {} =>
            /\ job \in objectPresent
            /\ rootCount[job] > 0

RetirementIsSafe ==
    /\ retired \subseteq {J1}
    /\ J0 \in objectPresent
    /\ (childAttached =>
           /\ J1 \in objectPresent
           /\ J1 \notin retired)
    /\ (J1 \in retired =>
           /\ ~childAttached
           /\ members[J1] = {}
           /\ Len(completions[J1]) = 0)
    /\ (J1 \notin objectPresent =>
           /\ J1 \in retired
           /\ handles[J1] = 0
           /\ ~childAttached)

SecurityInvariant ==
    /\ TypeOK
    /\ MembershipIsUnique
    /\ KernelRootsTrackMembers
    /\ ActiveMembersKeepJobsAlive
    /\ CompletionQueuesAreUnique
    /\ CompletionAccountingIsExact
    /\ BoundedRetention
    /\ TerminationHasProcessProvenance
    /\ FinalHandleCloseDoesNotReleaseMembers
    /\ RetirementIsSafe

=============================================================================
