# Endpoint IPC security invariants

The phase-three endpoint model makes the following security properties explicit:

1. A successful move-send consumes every moved source exactly when its message is
   committed to the bounded queue.
2. A failed send caused by queue exhaustion consumes no source authority and does
   not alter queue contents.
3. A successful copy-send retains the source and records the resulting additional
   authority as an explicit copy grant.
4. Receive removes only the FIFO head.
5. Receive installs every capability attached to the head or installs none of
   them.
6. Insufficient receive-handle capacity leaves the queued message untouched.
7. Authority can move between sender, queue, and receiver without appearing or
   disappearing; only an explicit copy grant may increase the number of live
   authority instances.
8. Duplicate source selection within one move operation is outside the legal
   transition set and is rejected by the live ABI before commit.

These properties refine security-constitution invariants 5, 6, 7, 13, and 14.
