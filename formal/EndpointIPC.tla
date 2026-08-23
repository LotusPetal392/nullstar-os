---- MODULE EndpointIPC ----
EXTENDS FiniteSets, Naturals, Sequences

\* Refinement of NullStar endpoint queue and capability-transfer semantics.
\*
\* This model intentionally omits byte payloads, endpoint peer closure,
\* scheduler blocking, wakeups, deadlines, and concrete handle encoding.  It
\* models the security-sensitive commit boundary: bounded FIFO enqueue,
\* copy-send, ownership-consuming move-send, and all-or-nothing receive.
\*
\* Invalid operations are represented by stuttering.  In particular, a send
\* against a full queue and a receive without enough destination handle capacity
\* cannot partially modify authority or queue state.

CONSTANTS
    C1, C2,
    CopyMode, MoveMode, PlainMode,
    MaxQueue,
    MaxMessages,
    MaxReceiverHandles,
    MaxCopyGrants

Caps == {C1, C2}
Modes == {CopyMode, MoveMode, PlainMode}
MessageIds == 1..MaxMessages
Messages == [id : MessageIds, caps : SUBSET Caps, mode : Modes]

ASSUME /\ C1 # C2
       /\ CopyMode # MoveMode
       /\ CopyMode # PlainMode
       /\ MoveMode # PlainMode
       /\ MaxQueue >= 1
       /\ MaxMessages > MaxQueue
       /\ MaxReceiverHandles >= 1
       /\ MaxCopyGrants >= 1

VARIABLES
    senderOwned,
    queue,
    receiverCount,
    receiverUsed,
    copyGrants,
    movedEver,
    nextId,
    sentOrder,
    receivedOrder

vars == <<
    senderOwned,
    queue,
    receiverCount,
    receiverUsed,
    copyGrants,
    movedEver,
    nextId,
    sentOrder,
    receivedOrder
>>

WellFormedMessage(message) ==
    \/ /\ message.mode = PlainMode
       /\ message.caps = {}
    \/ /\ message.mode \in {CopyMode, MoveMode}
       /\ message.caps # {}

QueueIds == [i \in 1..Len(queue) |-> queue[i].id]

QueuedCount(cap) ==
    Cardinality({i \in 1..Len(queue) : cap \in queue[i].caps})

AuthorityCount(cap) ==
    (IF cap \in senderOwned THEN 1 ELSE 0)
        + QueuedCount(cap)
        + receiverCount[cap]

CanSend ==
    /\ Len(queue) < MaxQueue
    /\ nextId <= MaxMessages

TypeOK ==
    /\ senderOwned \subseteq Caps
    /\ queue \in Seq(Messages)
    /\ Len(queue) <= MaxQueue
    /\ \A i \in 1..Len(queue): WellFormedMessage(queue[i])
    /\ receiverCount \in [Caps -> 0..MaxMessages]
    /\ receiverUsed \in 0..MaxReceiverHandles
    /\ copyGrants \in [Caps -> 0..MaxCopyGrants]
    /\ movedEver \subseteq Caps
    /\ nextId \in 1..(MaxMessages + 1)
    /\ sentOrder \in Seq(MessageIds)
    /\ receivedOrder \in Seq(MessageIds)
    /\ Len(sentOrder) <= MaxMessages
    /\ Len(receivedOrder) <= MaxMessages

Init ==
    /\ senderOwned = Caps
    /\ queue = <<>>
    /\ receiverCount = [cap \in Caps |-> 0]
    /\ receiverUsed = 0
    /\ copyGrants = [cap \in Caps |-> 0]
    /\ movedEver = {}
    /\ nextId = 1
    /\ sentOrder = <<>>
    /\ receivedOrder = <<>>

PlainSend ==
    /\ CanSend
    /\ LET message == [id |-> nextId, caps |-> {}, mode |-> PlainMode]
       IN /\ queue' = Append(queue, message)
          /\ sentOrder' = Append(sentOrder, nextId)
          /\ nextId' = nextId + 1
    /\ UNCHANGED <<senderOwned, receiverCount, receiverUsed, copyGrants, movedEver, receivedOrder>>

CopySend ==
    \E selected \in SUBSET Caps:
        /\ selected # {}
        /\ selected \subseteq senderOwned
        /\ CanSend
        /\ \A cap \in selected: copyGrants[cap] < MaxCopyGrants
        /\ LET message == [id |-> nextId, caps |-> selected, mode |-> CopyMode]
           IN /\ queue' = Append(queue, message)
              /\ sentOrder' = Append(sentOrder, nextId)
              /\ nextId' = nextId + 1
        /\ copyGrants' = [cap \in Caps |->
               IF cap \in selected THEN copyGrants[cap] + 1 ELSE copyGrants[cap]]
        /\ UNCHANGED <<senderOwned, receiverCount, receiverUsed, movedEver, receivedOrder>>

MoveSend ==
    \E selected \in SUBSET Caps:
        /\ selected # {}
        /\ selected \subseteq senderOwned
        /\ CanSend
        /\ LET message == [id |-> nextId, caps |-> selected, mode |-> MoveMode]
           IN /\ queue' = Append(queue, message)
              /\ sentOrder' = Append(sentOrder, nextId)
              /\ nextId' = nextId + 1
        /\ senderOwned' = senderOwned \ selected
        /\ movedEver' = movedEver \cup selected
        /\ UNCHANGED <<receiverCount, receiverUsed, copyGrants, receivedOrder>>

Receive ==
    /\ Len(queue) > 0
    /\ LET message == Head(queue)
           required == Cardinality(message.caps)
       IN /\ receiverUsed + required <= MaxReceiverHandles
          /\ queue' = Tail(queue)
          /\ receiverCount' = [cap \in Caps |->
                 IF cap \in message.caps THEN receiverCount[cap] + 1 ELSE receiverCount[cap]]
          /\ receiverUsed' = receiverUsed + required
          /\ receivedOrder' = Append(receivedOrder, message.id)
    /\ UNCHANGED <<senderOwned, copyGrants, movedEver, nextId, sentOrder>>

Next ==
    \/ PlainSend
    \/ CopySend
    \/ MoveSend
    \/ Receive

Spec == Init /\ [][Next]_vars

QueueBounded ==
    Len(queue) <= MaxQueue

\* Every successful send appends and every successful receive removes only the
\* head.  The messages already received followed by the live queue therefore
\* equal the complete successful-send order.
FifoDelivery ==
    sentOrder = receivedOrder \o QueueIds

\* A move changes where authority resides but not how much authority exists.
\* Receive likewise moves queued authority into the receiver.  Only an explicit
\* successful copy-send may increase the count, and copyGrants records exactly
\* that permitted provenance.
AuthorityHasProvenance ==
    \A cap \in Caps:
        AuthorityCount(cap) = 1 + copyGrants[cap]

\* Once a source has participated in a successful move-send, the sender cannot
\* regain that source inside this refinement layer.
MoveConsumesSource ==
    movedEver \cap senderOwned = {}

\* Receive installs every attachment from the dequeued message together.  A
\* partial install would make this exact accounting invariant fail.
ReceiverAccountingExact ==
    receiverUsed = receiverCount[C1] + receiverCount[C2]

SentIdsAreMonotonic ==
    /\ nextId = Len(sentOrder) + 1
    /\ \A i \in 1..Len(sentOrder): sentOrder[i] = i

SecurityInvariant ==
    /\ TypeOK
    /\ QueueBounded
    /\ FifoDelivery
    /\ AuthorityHasProvenance
    /\ MoveConsumesSource
    /\ ReceiverAccountingExact
    /\ SentIdsAreMonotonic

=============================================================================
