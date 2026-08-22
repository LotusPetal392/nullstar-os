---- MODULE CapabilityCore ----
EXTENDS FiniteSets, Naturals

\* Phase-one abstract security model for NullStar capability authority.
\*
\* The model intentionally ignores numeric syscall handles, kernel pointers,
\* scheduler state, endpoint queues, and object payloads.  A live handle is an
\* abstract authority token.  Later refinement models add generation-checked
\* slot reuse and queued IPC without changing the invariants established here.

CONSTANTS
    P1, P2,
    O1, O2,
    H1, H2, H3,
    UseRight, DuplicateRight, TransferRight

Processes == {P1, P2}
Objects == {O1, O2}
Handles == {H1, H2, H3}
Rights == {UseRight, DuplicateRight, TransferRight}

ASSUME /\ P1 # P2
       /\ O1 # O2
       /\ Cardinality(Handles) = 3
       /\ Cardinality(Rights) = 3

VARIABLES
    active,
    owner,
    objectOf,
    rightsOf,
    originObject,
    originRights

vars == <<active, owner, objectOf, rightsOf, originObject, originRights>>

TypeOK ==
    /\ active \subseteq Handles
    /\ owner \in [Handles -> Processes]
    /\ objectOf \in [Handles -> Objects]
    /\ rightsOf \in [Handles -> SUBSET Rights]
    /\ originObject \in [Handles -> Objects]
    /\ originRights \in [Handles -> SUBSET Rights]

\* Inactive slots have no security meaning.  Canonicalizing their metadata
\* avoids exploring states that differ only in unreachable stale model data.
InactiveCanonical ==
    \A h \in Handles \ active:
        /\ owner[h] = P1
        /\ objectOf[h] = O1
        /\ rightsOf[h] = {}
        /\ originObject[h] = O1
        /\ originRights[h] = {}

\* Two initial roots make both object identities reachable while leaving one
\* free handle slot for duplication, transfer, close, and reuse interleavings.
Init ==
    /\ active = {H1, H2}
    /\ owner = [h \in Handles |-> IF h = H2 THEN P2 ELSE P1]
    /\ objectOf = [h \in Handles |-> IF h = H2 THEN O2 ELSE O1]
    /\ rightsOf = [h \in Handles |-> IF h \in {H1, H2} THEN Rights ELSE {}]
    /\ originObject = objectOf
    /\ originRights = rightsOf

Close ==
    \E p \in Processes, src \in active:
        /\ owner[src] = p
        /\ active' = active \ {src}
        /\ owner' = [owner EXCEPT ![src] = P1]
        /\ objectOf' = [objectOf EXCEPT ![src] = O1]
        /\ rightsOf' = [rightsOf EXCEPT ![src] = {}]
        /\ originObject' = [originObject EXCEPT ![src] = O1]
        /\ originRights' = [originRights EXCEPT ![src] = {}]

Duplicate ==
    \E p \in Processes, src \in active:
        \E dst \in Handles \ active,
           requested \in SUBSET rightsOf[src]:
            /\ owner[src] = p
            /\ DuplicateRight \in rightsOf[src]
            /\ requested # {}
            /\ active' = active \cup {dst}
            /\ owner' = [owner EXCEPT ![dst] = p]
            /\ objectOf' = [objectOf EXCEPT ![dst] = objectOf[src]]
            /\ rightsOf' = [rightsOf EXCEPT ![dst] = requested]
            /\ originObject' = [originObject EXCEPT ![dst] = originObject[src]]
            /\ originRights' = [originRights EXCEPT ![dst] = originRights[src]]

\* Atomic rights replacement preserves the token's object identity and can
\* only attenuate authority.  The source must itself carry DUPLICATE authority,
\* matching NullStar's current replacement contract.
Replace ==
    \E p \in Processes, src \in active:
        \E requested \in SUBSET rightsOf[src]:
            /\ owner[src] = p
            /\ DuplicateRight \in rightsOf[src]
            /\ requested # {}
            /\ rightsOf' = [rightsOf EXCEPT ![src] = requested]
            /\ UNCHANGED <<active, owner, objectOf, originObject, originRights>>

\* This is the authority effect of a successful move-transfer.  Endpoint queue
\* commit and receive installation are introduced in EndpointIPC.tla later.
\* Failure is represented by stuttering: no partial authority transition exists.
MoveTransfer ==
    \E sourceProcess \in Processes,
       targetProcess \in Processes,
       src \in active:
        \E dst \in Handles \ active,
           requested \in SUBSET rightsOf[src]:
            /\ owner[src] = sourceProcess
            /\ TransferRight \in rightsOf[src]
            /\ requested # {}
            /\ active' = (active \ {src}) \cup {dst}
            /\ owner' = [owner EXCEPT ![src] = P1, ![dst] = targetProcess]
            /\ objectOf' = [objectOf EXCEPT ![src] = O1, ![dst] = objectOf[src]]
            /\ rightsOf' = [rightsOf EXCEPT ![src] = {}, ![dst] = requested]
            /\ originObject' = [originObject EXCEPT ![src] = O1,
                                                ![dst] = originObject[src]]
            /\ originRights' = [originRights EXCEPT ![src] = {},
                                              ![dst] = originRights[src]]

Next ==
    \/ Close
    \/ Duplicate
    \/ Replace
    \/ MoveTransfer

Spec == Init /\ [][Next]_vars

\* Security constitution, phase one.
NoEmptyAuthority ==
    \A h \in active: rightsOf[h] # {}

RightsNeverAmplify ==
    \A h \in active: rightsOf[h] \subseteq originRights[h]

ObjectIdentityStable ==
    \A h \in active: objectOf[h] = originObject[h]

BoundedAuthority ==
    Cardinality(active) <= Cardinality(Handles)

SecurityInvariant ==
    /\ TypeOK
    /\ InactiveCanonical
    /\ NoEmptyAuthority
    /\ RightsNeverAmplify
    /\ ObjectIdentityStable
    /\ BoundedAuthority

=============================================================================
