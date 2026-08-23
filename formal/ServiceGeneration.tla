---- MODULE ServiceGeneration ----
EXTENDS FiniteSets, Naturals

\* NullStar service-generation isolation model.
\*
\* Stable route authority is deliberately generation-neutral. A request may be
\* queued while one provider is active and complete after a replacement. The
\* accepted provider grant, however, is permanently bound to the generation and
\* fresh ingress object selected when the broker completes that request.

CONSTANTS
    R1, R2,
    H1, H2,
    O1, O2, O3,
    NoObject,
    NoGrant,
    MaxGeneration

Requests == {R1, R2}
Grants == {H1, H2}
Objects == {O1, O2, O3}
Generations == 1..MaxGeneration

ASSUME /\ Cardinality(Requests) = 2
       /\ Cardinality(Grants) = 2
       /\ Cardinality(Objects) = 3
       /\ NoObject \notin Objects
       /\ NoGrant \notin Grants
       /\ MaxGeneration = 3

VARIABLES
    retainedGeneration,
    activeGeneration,
    activeObject,
    generationObject,
    usedObjects,
    startedRequests,
    pendingRequests,
    completedRequests,
    requestStartGeneration,
    requestGrant,
    everIssued,
    liveGrants,
    grantGeneration,
    grantObject

vars == <<
    retainedGeneration,
    activeGeneration,
    activeObject,
    generationObject,
    usedObjects,
    startedRequests,
    pendingRequests,
    completedRequests,
    requestStartGeneration,
    requestGrant,
    everIssued,
    liveGrants,
    grantGeneration,
    grantObject
>>

PublishedGenerations ==
    {generation \in Generations : generationObject[generation] # NoObject}

Init ==
    /\ retainedGeneration = 0
    /\ activeGeneration = 0
    /\ activeObject = NoObject
    /\ generationObject = [generation \in Generations |-> NoObject]
    /\ usedObjects = {}
    /\ startedRequests = {}
    /\ pendingRequests = {}
    /\ completedRequests = {}
    /\ requestStartGeneration = [request \in Requests |-> 0]
    /\ requestGrant = [request \in Requests |-> NoGrant]
    /\ everIssued = {}
    /\ liveGrants = {}
    /\ grantGeneration = [grant \in Grants |-> 0]
    /\ grantObject = [grant \in Grants |-> NoObject]

\* Publishing a route generation consumes a fresh ingress object and must move
\* strictly beyond the retained generation tombstone. Replacing an active
\* provider simply changes what future resolutions select; already-issued grants
\* remain bound to their old object.
Publish ==
    \E generation \in Generations:
        \E object \in Objects:
            /\ generation > retainedGeneration
            /\ object \notin usedObjects
            /\ retainedGeneration' = generation
            /\ activeGeneration' = generation
            /\ activeObject' = object
            /\ generationObject' = [generationObject EXCEPT ![generation] = object]
            /\ usedObjects' = usedObjects \cup {object}
            /\ UNCHANGED <<
                   startedRequests, pendingRequests, completedRequests,
                   requestStartGeneration, requestGrant,
                   everIssued, liveGrants, grantGeneration, grantObject
               >>

\* Withdrawal requires the exact currently active generation in the live
\* implementation. The abstract action has no generation parameter because the
\* selected active generation is the only one it can withdraw. The tombstone and
\* generation-to-object history remain intact.
Withdraw ==
    /\ activeGeneration # 0
    /\ activeGeneration' = 0
    /\ activeObject' = NoObject
    /\ UNCHANGED <<
           retainedGeneration, generationObject, usedObjects,
           startedRequests, pendingRequests, completedRequests,
           requestStartGeneration, requestGrant,
           everIssued, liveGrants, grantGeneration, grantObject
       >>

\* A stable route request records the active provider at enqueue time only as
\* history. It does not acquire provider authority yet and therefore may legally
\* cross a replacement boundary before the broker resolves it.
BeginRequest ==
    \E request \in Requests \ startedRequests:
        /\ startedRequests' = startedRequests \cup {request}
        /\ pendingRequests' = pendingRequests \cup {request}
        /\ requestStartGeneration' =
               [requestStartGeneration EXCEPT ![request] = activeGeneration]
        /\ UNCHANGED <<
               retainedGeneration, activeGeneration, activeObject,
               generationObject, usedObjects, completedRequests,
               requestGrant, everIssued, liveGrants,
               grantGeneration, grantObject
           >>

\* Successful resolution binds one never-before-used abstract grant to exactly
\* the provider generation and ingress object active when the broker completes
\* the request.
CompleteRequestSuccess ==
    \E request \in pendingRequests:
        \E grant \in Grants \ everIssued:
            /\ activeGeneration # 0
            /\ pendingRequests' = pendingRequests \ {request}
            /\ completedRequests' = completedRequests \cup {request}
            /\ requestGrant' = [requestGrant EXCEPT ![request] = grant]
            /\ everIssued' = everIssued \cup {grant}
            /\ liveGrants' = liveGrants \cup {grant}
            /\ grantGeneration' =
                   [grantGeneration EXCEPT ![grant] = activeGeneration]
            /\ grantObject' = [grantObject EXCEPT ![grant] = activeObject]
            /\ UNCHANGED <<
                   retainedGeneration, activeGeneration, activeObject,
                   generationObject, usedObjects, startedRequests,
                   requestStartGeneration
               >>

\* If no provider is published when the broker processes a request, completion
\* is terminal but grants no provider capability.
CompleteRequestUnavailable ==
    \E request \in pendingRequests:
        /\ activeGeneration = 0
        /\ pendingRequests' = pendingRequests \ {request}
        /\ completedRequests' = completedRequests \cup {request}
        /\ UNCHANGED <<
               retainedGeneration, activeGeneration, activeObject,
               generationObject, usedObjects, startedRequests,
               requestStartGeneration, requestGrant,
               everIssued, liveGrants, grantGeneration, grantObject
           >>

\* Closing an issued provider handle revokes only that holder's live authority.
\* Historical binding remains so the model can prove that no grant identity was
\* rebound while it was live.
CloseGrant ==
    \E grant \in liveGrants:
        /\ liveGrants' = liveGrants \ {grant}
        /\ UNCHANGED <<
               retainedGeneration, activeGeneration, activeObject,
               generationObject, usedObjects,
               startedRequests, pendingRequests, completedRequests,
               requestStartGeneration, requestGrant,
               everIssued, grantGeneration, grantObject
           >>

Next ==
    \/ Publish
    \/ Withdraw
    \/ BeginRequest
    \/ CompleteRequestSuccess
    \/ CompleteRequestUnavailable
    \/ CloseGrant

Spec == Init /\ [][Next]_vars

TypeOK ==
    /\ retainedGeneration \in 0..MaxGeneration
    /\ activeGeneration \in 0..MaxGeneration
    /\ activeObject \in Objects \cup {NoObject}
    /\ generationObject \in [Generations -> Objects \cup {NoObject}]
    /\ usedObjects \subseteq Objects
    /\ startedRequests \subseteq Requests
    /\ pendingRequests \subseteq Requests
    /\ completedRequests \subseteq Requests
    /\ requestStartGeneration \in [Requests -> 0..MaxGeneration]
    /\ requestGrant \in [Requests -> Grants \cup {NoGrant}]
    /\ everIssued \subseteq Grants
    /\ liveGrants \subseteq Grants
    /\ grantGeneration \in [Grants -> 0..MaxGeneration]
    /\ grantObject \in [Grants -> Objects \cup {NoObject}]

RequestStateIsTerminal ==
    /\ pendingRequests \subseteq startedRequests
    /\ completedRequests \subseteq startedRequests
    /\ pendingRequests \cap completedRequests = {}
    /\ startedRequests = pendingRequests \cup completedRequests
    /\ \A request \in Requests \ startedRequests:
           /\ requestStartGeneration[request] = 0
           /\ requestGrant[request] = NoGrant
    /\ \A request \in pendingRequests:
           requestGrant[request] = NoGrant

\* Route-table tombstones preserve the newest generation ever published. Older
\* generations remain in history but can never become active again.
RetainedGenerationIsNewest ==
    /\ (retainedGeneration = 0) = (PublishedGenerations = {})
    /\ \A generation \in PublishedGenerations:
           generation <= retainedGeneration
    /\ retainedGeneration # 0 => retainedGeneration \in PublishedGenerations

ActivePublicationIsCurrent ==
    IF activeGeneration = 0
    THEN activeObject = NoObject
    ELSE /\ activeGeneration = retainedGeneration
         /\ activeGeneration \in PublishedGenerations
         /\ activeObject = generationObject[activeGeneration]

\* Each provider generation consumes a different ingress object. This is the
\* object-identity boundary that makes replacement isolation possible.
FreshIngressPerGeneration ==
    \A first \in PublishedGenerations:
        \A second \in PublishedGenerations:
            first # second => generationObject[first] # generationObject[second]

UsedObjectHistoryIsExact ==
    usedObjects = {object \in Objects :
        \E generation \in PublishedGenerations:
            generationObject[generation] = object}

GrantStateIsCanonical ==
    /\ liveGrants \subseteq everIssued
    /\ \A grant \in Grants \ everIssued:
           /\ grantGeneration[grant] = 0
           /\ grantObject[grant] = NoObject
    /\ \A grant \in everIssued:
           /\ grantGeneration[grant] \in PublishedGenerations
           /\ grantGeneration[grant] # 0
           /\ grantObject[grant] = generationObject[grantGeneration[grant]]

\* This is the central provider-replacement invariant. If a live provider grant
\* was issued for an older generation, a later active provider must be a different
\* ingress object. The old grant never starts reaching the replacement.
OldAuthorityNeverRebinds ==
    \A grant \in liveGrants:
        IF activeGeneration = 0 \/ grantGeneration[grant] = activeGeneration
        THEN TRUE
        ELSE grantObject[grant] # activeObject

\* Every successful request response points to one historically issued grant;
\* unavailable completions carry NoGrant. A queued request may have started in a
\* different generation, but the returned grant itself has exact generation and
\* object provenance.
ResolutionHasExactProvenance ==
    \A request \in completedRequests:
        requestGrant[request] = NoGrant
        \/ /\ requestGrant[request] \in everIssued
            /\ grantObject[requestGrant[request]] =
                   generationObject[grantGeneration[requestGrant[request]]]

SecurityInvariant ==
    /\ TypeOK
    /\ RequestStateIsTerminal
    /\ RetainedGenerationIsNewest
    /\ ActivePublicationIsCurrent
    /\ FreshIngressPerGeneration
    /\ UsedObjectHistoryIsExact
    /\ GrantStateIsCanonical
    /\ OldAuthorityNeverRebinds
    /\ ResolutionHasExactProvenance

=============================================================================
