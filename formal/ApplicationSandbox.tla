---- MODULE ApplicationSandbox ----
EXTENDS FiniteSets

\* NullStar target-state application sandbox model.
\*
\* This phase composes the earlier capability, containment, and service-generation
\* rules at the application-policy boundary.  It deliberately models the accepted
\* future application-manager architecture rather than claiming that the current
\* launcher already implements these transitions.
\*
\* P1 is the main component of application A1.  P2 is a reduced child component
\* of A1.  P3 is the main component of application A2.  Static application
\* identity is descriptive; authority appears only through trusted bootstrap,
\* broker issuance, portal mediation, or explicit same-application delegation.

CONSTANTS
    P1, P2, P3,
    A1, A2,
    RPrivate, RPortal, RSensitive, RDocument,
    OriginBootstrap, OriginBroker, OriginPortal, OriginParent,
    NoOrigin

Processes == {P1, P2, P3}
RootProcesses == {P1, P3}
Applications == {A1, A2}
Resources == {RPrivate, RPortal, RSensitive, RDocument}
Origins == {OriginBootstrap, OriginBroker, OriginPortal, OriginParent}

ASSUME /\ Cardinality(Processes) = 3
       /\ Cardinality(RootProcesses) = 2
       /\ Cardinality(Applications) = 2
       /\ Cardinality(Resources) = 4
       /\ Cardinality(Origins) = 4
       /\ NoOrigin \notin Origins

AppOf(process) ==
    IF process = P1 \/ process = P2
    THEN A1
    ELSE A2

\* The ceiling is the already-computed intersection of platform maximum,
\* sandbox profile, signed entitlement, declaration, and administrator policy.
\* A1's main component may become eligible for the sensitive resource.  A2 may
\* not, and A1's reduced child may receive only private/document authority.
Ceiling(process) ==
    IF process = P1
    THEN Resources
    ELSE IF process = P2
         THEN {RPrivate, RDocument}
         ELSE {RPrivate, RPortal, RDocument}

\* Only trusted root launch receives baseline authority.  Merely having an
\* application identity does not create these capabilities.
Baseline(process) ==
    IF process \in RootProcesses
    THEN {RPrivate, RPortal} \cap Ceiling(process)
    ELSE {}

\* Direct handle delegation is intentionally much narrower than the parent's
\* complete authority.  Documents cross component/application boundaries through
\* the trusted portal path instead.
DirectDelegable == {RPrivate}

VARIABLES
    live,
    authority,
    origin,
    requested,
    approved,
    brokerGranted,
    portalGranted,
    delegated

vars == <<
    live,
    authority,
    origin,
    requested,
    approved,
    brokerGranted,
    portalGranted,
    delegated
>>

EmptyOrigin == [resource \in Resources |-> NoOrigin]

Init ==
    /\ live = {}
    /\ authority = [process \in Processes |-> {}]
    /\ origin = [process \in Processes |-> EmptyOrigin]
    /\ requested = [process \in Processes |-> {}]
    /\ approved = [process \in Processes |-> {}]
    /\ brokerGranted = [process \in Processes |-> {}]
    /\ portalGranted = [process \in Processes |-> {}]
    /\ delegated = [process \in Processes |-> {}]

\* Trusted application-manager launch installs only the baseline allowlist.
\* Application identity, manifest strings, package path, and display metadata are
\* not modeled as authority-bearing inputs.
LaunchRoot ==
    \E process \in RootProcesses \ live:
        /\ live' = live \cup {process}
        /\ authority' = [authority EXCEPT ![process] = Baseline(process)]
        /\ origin' =
               [origin EXCEPT ![process] =
                   [resource \in Resources |->
                       IF resource \in Baseline(process)
                       THEN OriginBootstrap
                       ELSE NoOrigin]]
        /\ UNCHANGED <<
               requested, approved, brokerGranted, portalGranted, delegated
           >>

\* The reduced A1 child is created through an explicit handle allowlist.  It does
\* not clone the parent's table.  Only already-held, directly delegable authority
\* inside the child's fixed ceiling may be installed at creation.
SpawnChild ==
    /\ P1 \in live
    /\ P2 \notin live
    /\ \E allow \in SUBSET (authority[P1] \cap DirectDelegable \cap Ceiling(P2)):
           /\ live' = live \cup {P2}
           /\ authority' = [authority EXCEPT ![P2] = allow]
           /\ origin' =
                  [origin EXCEPT ![P2] =
                      [resource \in Resources |->
                          IF resource \in allow
                          THEN OriginParent
                          ELSE NoOrigin]]
           /\ delegated' = [delegated EXCEPT ![P2] = allow]
           /\ UNCHANGED <<
                  requested, approved, brokerGranted, portalGranted
              >>

\* A manifest/runtime request is policy input only.  Recording the request does
\* not create a capability.
DeclareSensitive ==
    \E process \in live:
        /\ RSensitive \in Ceiling(process)
        /\ RSensitive \notin requested[process]
        /\ requested' =
               [requested EXCEPT ![process] = @ \cup {RSensitive}]
        /\ UNCHANGED <<
               live, authority, origin, approved,
               brokerGranted, portalGranted, delegated
           >>

\* This abstracts the independent policy/user decision.  Approval still is not
\* authority; broker issuance is a separate transition.
ApproveSensitive ==
    \E process \in live:
        /\ RSensitive \in requested[process]
        /\ RSensitive \notin approved[process]
        /\ approved' = [approved EXCEPT ![process] = @ \cup {RSensitive}]
        /\ UNCHANGED <<
               live, authority, origin, requested,
               brokerGranted, portalGranted, delegated
           >>

\* A broker/provider may issue only authority inside the fixed process ceiling and
\* only after both declaration and approval.  The issued authority has explicit
\* broker provenance.
BrokerGrant ==
    \E process \in live:
        /\ RSensitive \in Ceiling(process)
        /\ RSensitive \in requested[process]
        /\ RSensitive \in approved[process]
        /\ RSensitive \notin authority[process]
        /\ authority' =
               [authority EXCEPT ![process] = @ \cup {RSensitive}]
        /\ origin' =
               [origin EXCEPT ![process][RSensitive] = OriginBroker]
        /\ brokerGranted' =
               [brokerGranted EXCEPT ![process] = @ \cup {RSensitive}]
        /\ UNCHANGED <<
               live, requested, approved, portalGranted, delegated
           >>

\* A trusted portal can issue one exact user-selected document capability to a
\* process that has portal authority and whose profile admits that resource.
PortalAcquire ==
    \E process \in live:
        /\ RPortal \in authority[process]
        /\ RDocument \in Ceiling(process)
        /\ RDocument \notin authority[process]
        /\ authority' =
               [authority EXCEPT ![process] = @ \cup {RDocument}]
        /\ origin' =
               [origin EXCEPT ![process][RDocument] = OriginPortal]
        /\ portalGranted' =
               [portalGranted EXCEPT ![process] = @ \cup {RDocument}]
        /\ UNCHANGED <<
               live, requested, approved, brokerGranted, delegated
           >>

\* Cross-component and cross-application document transfer is mediated by the
\* trusted portal.  The target receives a new scoped grant; there is no direct
\* arbitrary-process capability-copy action in this model.
PortalTransfer ==
    \E source \in live:
        \E target \in live:
            /\ source # target
            /\ RPortal \in authority[source]
            /\ RDocument \in authority[source]
            /\ RDocument \in Ceiling(target)
            /\ RDocument \notin authority[target]
            /\ authority' =
                   [authority EXCEPT ![target] = @ \cup {RDocument}]
            /\ origin' =
                   [origin EXCEPT ![target][RDocument] = OriginPortal]
            /\ portalGranted' =
                   [portalGranted EXCEPT ![target] = @ \cup {RDocument}]
            /\ UNCHANGED <<
                   live, requested, approved, brokerGranted, delegated
               >>

\* Direct same-application delegation is intentionally restricted to a resource
\* class whose transfer policy permits it, and only from A1's main component to
\* its declared reduced child.
DelegateSameApplication ==
    /\ P1 \in live
    /\ P2 \in live
    /\ AppOf(P1) = AppOf(P2)
    /\ RPrivate \in authority[P1]
    /\ RPrivate \in DirectDelegable
    /\ RPrivate \in Ceiling(P2)
    /\ RPrivate \notin authority[P2]
    /\ authority' = [authority EXCEPT ![P2] = @ \cup {RPrivate}]
    /\ origin' = [origin EXCEPT ![P2][RPrivate] = OriginParent]
    /\ delegated' = [delegated EXCEPT ![P2] = @ \cup {RPrivate}]
    /\ UNCHANGED <<
           live, requested, approved, brokerGranted, portalGranted
       >>

\* Dropping one capability removes only current authority.  Historical issuance
\* sets remain so the model can show that future authority still requires a new
\* permitted acquisition transition rather than identity or stale metadata.
DropAuthority ==
    \E process \in live:
        \E resource \in authority[process]:
            /\ authority' =
                   [authority EXCEPT ![process] = @ \ {resource}]
            /\ origin' = [origin EXCEPT ![process][resource] = NoOrigin]
            /\ UNCHANGED <<
                   live, requested, approved,
                   brokerGranted, portalGranted, delegated
               >>

Next ==
    \/ LaunchRoot
    \/ SpawnChild
    \/ DeclareSensitive
    \/ ApproveSensitive
    \/ BrokerGrant
    \/ PortalAcquire
    \/ PortalTransfer
    \/ DelegateSameApplication
    \/ DropAuthority

Spec == Init /\ [][Next]_vars

TypeOK ==
    /\ live \subseteq Processes
    /\ authority \in [Processes -> SUBSET Resources]
    /\ origin \in [Processes -> [Resources -> Origins \cup {NoOrigin}]]
    /\ requested \in [Processes -> SUBSET Resources]
    /\ approved \in [Processes -> SUBSET Resources]
    /\ brokerGranted \in [Processes -> SUBSET Resources]
    /\ portalGranted \in [Processes -> SUBSET Resources]
    /\ delegated \in [Processes -> SUBSET Resources]

InactiveStateIsCanonical ==
    \A process \in Processes \ live:
        /\ authority[process] = {}
        /\ origin[process] = EmptyOrigin
        /\ requested[process] = {}
        /\ approved[process] = {}
        /\ brokerGranted[process] = {}
        /\ portalGranted[process] = {}
        /\ delegated[process] = {}

\* No acquisition path may exceed the immutable profile/policy ceiling assigned
\* to that process role.
AuthorityWithinCeiling ==
    \A process \in live:
        authority[process] \subseteq Ceiling(process)

\* Identity, manifest declarations, and approvals are never sufficient by
\* themselves.  Every live resource is either trusted baseline authority or has
\* an explicit grant history produced by one of the modeled issuers.
AuthorityHasExplicitProvenance ==
    \A process \in live:
        authority[process]
            \subseteq Baseline(process)
                       \cup brokerGranted[process]
                       \cup portalGranted[process]
                       \cup delegated[process]

OriginMatchesAuthority ==
    \A process \in Processes:
        \A resource \in Resources:
            /\ (resource \in authority[process])
                   = (origin[process][resource] # NoOrigin)
            /\ (origin[process][resource] = OriginBootstrap
                   => resource \in Baseline(process))
            /\ (origin[process][resource] = OriginBroker
                   => resource \in brokerGranted[process])
            /\ (origin[process][resource] = OriginPortal
                   => resource \in portalGranted[process])
            /\ (origin[process][resource] = OriginParent
                   => resource \in delegated[process])

\* Declaration and approval can make sensitive authority eligible, but until the
\* broker actually issues it the resource is absent.
ManifestAndApprovalAreNotAuthority ==
    \A process \in live:
        RSensitive \notin brokerGranted[process]
            => RSensitive \notin authority[process]

BrokerGrantRespectsPolicy ==
    \A process \in Processes:
        /\ requested[process] \subseteq {RSensitive}
        /\ approved[process] \subseteq requested[process]
        /\ brokerGranted[process] \subseteq approved[process] \cap Ceiling(process)

\* Documents never arrive through ambient namespace access or direct application
\* laundering.  Every live document capability is portal-issued.
DocumentsRequirePortalMediation ==
    \A process \in live:
        RDocument \in authority[process]
            => origin[process][RDocument] = OriginPortal

\* Direct parent delegation is same-application, reduced, and confined to the
\* declared child.  In particular neither the second application nor the main
\* component can acquire authority merely because another process possesses it.
DirectDelegationIsReduced ==
    /\ delegated[P1] = {}
    /\ delegated[P3] = {}
    /\ delegated[P2] \subseteq DirectDelegable \cap Ceiling(P2)
    /\ AppOf(P1) = AppOf(P2)
    /\ AppOf(P1) # AppOf(P3)

\* The reduced renderer/helper profile never receives the main component's portal
\* or sensitive broker authority.  A selected document can reach it only through
\* a later explicit portal-mediated transfer.
ChildProfileCannotRelax ==
    P2 \in live
        => /\ RPortal \notin authority[P2]
           /\ RSensitive \notin authority[P2]
           /\ authority[P2] \subseteq Ceiling(P2)

\* A2's fixed ceiling excludes the sensitive class entirely.  Knowing the same
\* permission/resource name or copying a manifest cannot change that fact.
RestrictedAuthorityCannotBeSelfGranted ==
    P3 \in live => RSensitive \notin authority[P3]

SecurityInvariant ==
    /\ TypeOK
    /\ InactiveStateIsCanonical
    /\ AuthorityWithinCeiling
    /\ AuthorityHasExplicitProvenance
    /\ OriginMatchesAuthority
    /\ ManifestAndApprovalAreNotAuthority
    /\ BrokerGrantRespectsPolicy
    /\ DocumentsRequirePortalMediation
    /\ DirectDelegationIsReduced
    /\ ChildProfileCannotRelax
    /\ RestrictedAuthorityCannotBeSelfGranted

=============================================================================
