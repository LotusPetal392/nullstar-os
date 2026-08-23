---- MODULE JobContainment ----
EXTENDS FiniteSets, Naturals

\* NullStar job-containment safety model.
\*
\* This layer models only immutable job ancestry, one-way process assignment,
\* fork inheritance, and tightening-only subtree admission policy. Job exit
\* records, termination, drainage, retirement, capability-handle lifetime, and
\* scheduler behavior are intentionally deferred to a later JobLifecycle model.

CONSTANTS
    P0, P1, P2,
    J0, J1, J2,
    NoProcess,
    NoJob,
    MaxLimit

Processes == {P0, P1, P2}
Jobs == {J0, J1, J2}

ASSUME /\ Cardinality(Processes) = 3
       /\ Cardinality(Jobs) = 3
       /\ NoProcess \notin Processes
       /\ NoJob \notin Jobs
       /\ MaxLimit \in Nat
       /\ MaxLimit >= 1

VARIABLES
    liveJobs,
    parentJob,
    ancestors,
    processLimit,
    tightestLimit,
    liveProcesses,
    processParent,
    jobOf,
    requiredEver

vars == <<
    liveJobs,
    parentJob,
    ancestors,
    processLimit,
    tightestLimit,
    liveProcesses,
    processParent,
    jobOf,
    requiredEver
>>

CurrentContainment(process) ==
    IF jobOf[process] = NoJob THEN {} ELSE ancestors[jobOf[process]]

MembersUnder(job) ==
    {process \in liveProcesses :
        IF jobOf[process] = NoJob
        THEN FALSE
        ELSE job \in ancestors[jobOf[process]]}

\* A job's own limit and every ancestor limit gate new membership anywhere in
\* that subtree. Tightening a limit below the current population is permitted;
\* it prevents later admissions rather than ejecting existing members.
CanAdmit(job) ==
    IF job \notin liveJobs
    THEN FALSE
    ELSE \A ancestor \in ancestors[job]:
             Cardinality(MembersUnder(ancestor)) < processLimit[ancestor]

Init ==
    /\ liveJobs = {J0}
    /\ parentJob = [job \in Jobs |-> NoJob]
    /\ ancestors = [job \in Jobs |-> IF job = J0 THEN {J0} ELSE {}]
    /\ processLimit = [job \in Jobs |-> MaxLimit]
    /\ tightestLimit = processLimit
    /\ liveProcesses = {P0, P1}
    /\ processParent = [process \in Processes |->
           IF process = P1 THEN P0 ELSE NoProcess]
    /\ jobOf = [process \in Processes |-> NoJob]
    /\ requiredEver = [process \in Processes |-> {}]

\* A child job is created once beneath an already-live parent. Its full ancestor
\* closure is fixed at creation; there is no reparent transition.
CreateChildJob ==
    \E parent \in liveJobs:
        \E child \in Jobs \ liveJobs:
            /\ child \notin ancestors[parent]
            /\ liveJobs' = liveJobs \cup {child}
            /\ parentJob' = [parentJob EXCEPT ![child] = parent]
            /\ ancestors' = [ancestors EXCEPT
                   ![child] = ancestors[parent] \cup {child}]
            /\ processLimit' = [processLimit EXCEPT ![child] = MaxLimit]
            /\ tightestLimit' = [tightestLimit EXCEPT ![child] = MaxLimit]
            /\ UNCHANGED <<liveProcesses, processParent, jobOf, requiredEver>>

\* Policy can stay the same or become stricter, never larger.
TightenLimit ==
    \E job \in liveJobs:
        \E newLimit \in 0..processLimit[job]:
            /\ processLimit' = [processLimit EXCEPT ![job] = newLimit]
            /\ tightestLimit' = [tightestLimit EXCEPT ![job] = newLimit]
            /\ UNCHANGED <<
                   liveJobs, parentJob, ancestors,
                   liveProcesses, processParent, jobOf, requiredEver
               >>

\* The live kernel restricts assignment to a live direct child that has no job.
\* Once assigned, this model deliberately provides no move or unassign action.
AssignDirectChild ==
    \E actor \in liveProcesses:
        \E child \in liveProcesses:
            \E job \in liveJobs:
                /\ actor # child
                /\ processParent[child] = actor
                /\ jobOf[child] = NoJob
                /\ CanAdmit(job)
                /\ jobOf' = [jobOf EXCEPT ![child] = job]
                /\ requiredEver' = [requiredEver EXCEPT
                       ![child] = @ \cup ancestors[job]]
                /\ UNCHANGED <<
                       liveJobs, parentJob, ancestors,
                       processLimit, tightestLimit,
                       liveProcesses, processParent
                   >>

\* Fork either preserves the absence of containment or inherits the parent's
\* exact current job. A contained fork must satisfy every ancestor admission
\* limit before the child becomes live.
ForkProcess ==
    \E parent \in liveProcesses:
        \E child \in Processes \ liveProcesses:
            /\ (jobOf[parent] = NoJob \/ CanAdmit(jobOf[parent]))
            /\ liveProcesses' = liveProcesses \cup {child}
            /\ processParent' = [processParent EXCEPT ![child] = parent]
            /\ jobOf' = [jobOf EXCEPT ![child] = jobOf[parent]]
            /\ requiredEver' = [requiredEver EXCEPT
                   ![child] = CurrentContainment(parent)]
            /\ UNCHANGED <<
                   liveJobs, parentJob, ancestors,
                   processLimit, tightestLimit
               >>

Next ==
    \/ CreateChildJob
    \/ TightenLimit
    \/ AssignDirectChild
    \/ ForkProcess

Spec == Init /\ [][Next]_vars

TypeOK ==
    /\ liveJobs \subseteq Jobs
    /\ parentJob \in [Jobs -> Jobs \cup {NoJob}]
    /\ ancestors \in [Jobs -> SUBSET Jobs]
    /\ processLimit \in [Jobs -> 0..MaxLimit]
    /\ tightestLimit \in [Jobs -> 0..MaxLimit]
    /\ liveProcesses \subseteq Processes
    /\ processParent \in [Processes -> Processes \cup {NoProcess}]
    /\ jobOf \in [Processes -> Jobs \cup {NoJob}]
    /\ requiredEver \in [Processes -> SUBSET Jobs]
    /\ \A job \in Jobs \ liveJobs:
           /\ parentJob[job] = NoJob
           /\ ancestors[job] = {}
           /\ processLimit[job] = MaxLimit
           /\ tightestLimit[job] = MaxLimit
    /\ \A process \in Processes \ liveProcesses:
           /\ processParent[process] = NoProcess
           /\ jobOf[process] = NoJob
           /\ requiredEver[process] = {}

RootIsStable ==
    /\ J0 \in liveJobs
    /\ parentJob[J0] = NoJob
    /\ ancestors[J0] = {J0}

HierarchyClosure ==
    \A job \in liveJobs:
        IF job = J0
        THEN /\ parentJob[job] = NoJob
             /\ ancestors[job] = {J0}
        ELSE /\ parentJob[job] \in liveJobs
             /\ ancestors[job] = ancestors[parentJob[job]] \cup {job}

HierarchyIsAcyclic ==
    \A job \in liveJobs \ {J0}:
        job \notin ancestors[parentJob[job]]

AncestorsRemainLive ==
    \A job \in liveJobs:
        ancestors[job] \subseteq liveJobs

AssignedProcessesUseLiveJobs ==
    \A process \in liveProcesses:
        jobOf[process] = NoJob \/ jobOf[process] \in liveJobs

\* Historical containment is monotonic. Direct assignment records the chosen
\* job's full ancestor closure; fork records the parent's current containment.
\* No later state may provide less containment than has already been established.
ContainmentNeverRelaxes ==
    \A process \in liveProcesses:
        requiredEver[process] \subseteq CurrentContainment(process)

\* tightestLimit is history state: it records the smallest accepted limit so a
\* future relaxation transition would violate this equality.
LimitsNeverRelax ==
    processLimit = tightestLimit

SecurityInvariant ==
    /\ TypeOK
    /\ RootIsStable
    /\ HierarchyClosure
    /\ HierarchyIsAcyclic
    /\ AncestorsRemainLive
    /\ AssignedProcessesUseLiveJobs
    /\ ContainmentNeverRelaxes
    /\ LimitsNeverRelax

=============================================================================
