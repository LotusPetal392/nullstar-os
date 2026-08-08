# NullStar service-definition format

## Status

Version 1 of the bounded service-definition file format and its allocation-free parser are
implemented by `crates/service-definition`. Loading these files from `/System/services`,
resolving policy, and activating services are separate milestones and are not implemented
by the format crate.

Packaged machine-service definitions belong below `/System/services`. Machine enablement
and local policy remain separate data below `/System/config/services`; a packaged
definition is never modified to record whether a service is enabled.

## File envelope

A version 1 definition is a UTF-8 file of at most 4096 bytes. It uses LF line endings,
must end with LF, and begins with this exact line:

```text
NullStar Service Definition 1
```

Subsequent nonempty lines are either:

```text
Key=Value
# Comment text
```

A comment marker is recognized only as `# ` at the beginning of a line. Carriage returns,
empty keys or values, malformed lines, duplicate scalar fields, and unknown fields are
rejected. Field order is not significant. `Argument` is the only repeatable field.
Malformed-line and unknown-field diagnostics include the one-based source line number.

Rejecting unknown fields is intentional: version 1 has no optional extension namespace,
so a manager cannot silently ignore policy that the author may have expected it to
enforce. A later compatible extension requires an explicitly specified mechanism or a new
format version.

## Fields

| Field | Cardinality | Meaning |
| --- | --- | --- |
| `ServiceId` | exactly one | Canonical lowercase RFC-order UUIDv4 service identity |
| `Name` | exactly one | Stable dotted canonical name, at most 63 bytes |
| `Description` | exactly one | Human-readable control-free UTF-8, at most 256 bytes |
| `Executable` | exactly one | Absolute canonical executable path, at most 192 bytes |
| `Argument` | zero to 16 | One nonempty control-free structured argument, at most 256 bytes |
| `Readiness` | exactly one | `immediate` or `notify` |
| `ReadyMessage` | conditional | Nonempty control-free exact notify message, at most 128 bytes |
| `Restart` | exactly one | `never`, `on-failure`, or `always` |
| `RestartLimit` | exactly one | Canonical decimal integer from 0 through 16 |
| `RestartBackoffYields` | exactly one | Canonical decimal integer from 0 through 1,000,000 |

A canonical service name consists of dot-separated components. Each component begins with
a lowercase ASCII letter, ends with a lowercase ASCII letter or digit, and otherwise
contains only lowercase ASCII letters, digits, or hyphens.

`Executable` begins with `/`, does not end with `/`, and contains no empty, `.` or `..`
component, ASCII control, or ASCII whitespace. Path acceptance is only syntactic;
installation and activation policy must separately authorize the package, executable,
namespace, and service class.

Arguments are represented by repeated fields rather than one shell command. Spaces inside
one `Argument` value remain part of that argument. Definitions are never evaluated by a
shell.

`Readiness=notify` requires exactly one nonempty `ReadyMessage`.
`Readiness=immediate` forbids `ReadyMessage`. The manager, not the definition, chooses and
installs any bootstrap handle used to deliver readiness.

`Restart=never` requires both numeric restart fields to be zero. `on-failure` and `always`
require a nonzero `RestartLimit`. These bounded counters do not by themselves define a
complete production rate-limit window, watchdog, or escalation policy.

## Example

```text
NullStar Service Definition 1
ServiceId=4c71a3aa-bc2c-4b38-8db4-737e0369ef8c
Name=system.definition-probe
Description=Definition-backed activation probe
Executable=/System/bin/service-definition-probe
Argument=--mode
Argument=service activation
Readiness=notify
ReadyMessage=service-definition-probe: ready
Restart=on-failure
RestartLimit=3
RestartBackoffYields=32
```

## Trust and authority

A valid definition is untrusted declarative input. Parsing it does not:

- prove package or publisher identity;
- grant filesystem, endpoint, device, logging, or administrative capabilities;
- authorize a privileged service class;
- select machine enablement;
- make an executable trusted;
- bypass namespace or executable-loader validation.

The future service manager must combine a parsed definition with package metadata,
machine policy, explicit capability routes, and a manager-owned generation before launch.
PID 1 retains the independent bootstrap and recovery services required to mount and read
`/System/services`.

## Version 1 exclusions

Version 1 deliberately does not encode dependencies, ordering, environment variables,
working-directory authority, channel activation, capability requests, devices, job
policy, resource budgets, watchdogs, shutdown deadlines, or health checks. Those fields
should be assigned only alongside executable lifecycle and authorization semantics rather
than accepted speculatively by the parser.
