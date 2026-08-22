---- MODULE HandleGeneration ----
EXTENDS FiniteSets, Naturals

\* Refinement of NullStar capability identity across table-slot reuse.
\*
\* A userspace handle is modeled as <<slot, generation>>.  Closing a live
\* handle retires that exact pair and advances the slot generation.  The final
\* representable generation advances to 0, which means permanently exhausted:
\* the slot cannot be allocated again and no generation ever wraps.

CONSTANTS
    S1, S2,
    O1, O2,
    MaxGeneration

Slots == {S1, S2}
Objects == {O1, O2}
LiveGenerations == 1..MaxGeneration
Handles == {<<s, g>> : s \in Slots, g \in LiveGenerations}

ASSUME /\ S1 # S2
       /\ O1 # O2
       /\ MaxGeneration >= 2

VARIABLES
    active,
    generation,
    objectOf,
    retired

vars == <<active, generation, objectOf, retired>>

CurrentHandle(slot) == <<slot, generation[slot]>>

TypeOK ==
    /\ active \subseteq Slots
    /\ generation \in [Slots -> 0..MaxGeneration]
    /\ objectOf \in [Slots -> Objects]
    /\ retired \subseteq Handles
    /\ \A slot \in active: generation[slot] \in LiveGenerations
    /\ \A slot \in Slots \ active: objectOf[slot] = O1

\* Start with one live handle and one never-used free slot.  The exact object
\* assignment is descriptive state only; authority remains the live handle.
Init ==
    /\ active = {S1}
    /\ generation = [slot \in Slots |-> 1]
    /\ objectOf = [slot \in Slots |-> IF slot = S1 THEN O2 ELSE O1]
    /\ retired = {}

Open ==
    \E slot \in Slots \ active:
        /\ generation[slot] # 0
        /\ \E object \in Objects:
            /\ active' = active \cup {slot}
            /\ objectOf' = [objectOf EXCEPT ![slot] = object]
            /\ UNCHANGED <<generation, retired>>

Close ==
    \E slot \in active:
        LET oldHandle == CurrentHandle(slot)
        IN /\ active' = active \ {slot}
           /\ generation' = [generation EXCEPT
                  ![slot] = IF @ = MaxGeneration THEN 0 ELSE @ + 1]
           /\ objectOf' = [objectOf EXCEPT ![slot] = O1]
           /\ retired' = retired \cup {oldHandle}

Next ==
    \/ Open
    \/ Close

Spec == Init /\ [][Next]_vars

\* Security constitution invariant 8: a retired opaque handle never resolves
\* again, even after its table slot is reused for another object.
NoStaleHandleRevival ==
    \A oldHandle \in retired:
        \A slot \in active:
            oldHandle # CurrentHandle(slot)

\* Every retired generation for a slot remains strictly behind the current
\* generation, unless the slot has exhausted the generation space entirely.
RetiredGenerationNeverReturns ==
    \A oldHandle \in retired:
        LET slot == oldHandle[1]
            oldGeneration == oldHandle[2]
        IN generation[slot] = 0 \/ generation[slot] > oldGeneration

ExhaustedSlotsStayClosed ==
    \A slot \in Slots:
        generation[slot] = 0 => slot \notin active

LiveHandlesAreWellFormed ==
    \A slot \in active:
        CurrentHandle(slot) \in Handles

SecurityInvariant ==
    /\ TypeOK
    /\ NoStaleHandleRevival
    /\ RetiredGenerationNeverReturns
    /\ ExhaustedSlotsStayClosed
    /\ LiveHandlesAreWellFormed

=============================================================================
