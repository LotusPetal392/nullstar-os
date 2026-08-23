# EndpointIPC model

`EndpointIPC.tla` checks the bounded authority-transfer semantics of the current
endpoint IPC design. It is a refinement layer above `CapabilityCore.tla` and
`HandleGeneration.tla`, not a replacement for either model.

The configured model uses two source capabilities, a queue bound of two
messages, four total successful message IDs, receiver capacity for two attached
handles, and at most two explicit copy grants per source. Those small bounds are
chosen so TLC can exhaustively explore queue-full, move, copy, receive, and
receiver-capacity interactions while keeping counterexamples compact.

Important checked properties are:

- FIFO delivery;
- bounded queue occupancy;
- ownership-consuming successful move-send;
- exact receiver attachment accounting;
- authority conservation except for explicit successful copy grants;
- monotonic successful message IDs.

Invalid operations are represented by stuttering. This includes attempts to send
when the queue is full and attempts to receive the FIFO head when the receiver
cannot install all attached capabilities.
