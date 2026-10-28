# Windows local discovery

Finds the BlockOps Fabric mod running on the same computer. The mod writes one descriptor per
game process to `%LOCALAPPDATA%\BlockOps\discovery\minecraft-<pid>.json`; the desktop calls
`discover` to list the live ones and then authenticates to each bridge with HMAC (see
`blockops_coordination::local_bridge`). The descriptor is a bearer capability and must never be
logged, shown in the interface or passed on a command line.

On Windows the reader opens a file without following its final reparse point,
checks its owner and a protected, owner-only DACL on the open handle, and reads
at most 4097 bytes. It fails closed for other ACLs. This protects against other
Windows users, not software already running as the same user or an administrator.
`discover` also drops expired descriptors, implausibly long lifetimes and those whose
process has exited. A reused process id can still pass; the bridge's response proof,
not the descriptor, decides whether the desktop trusts an endpoint.

The mod's writer and this reader are tested against each other on Windows CI, including
a rejected file with inherited permissions. Stale files, hostile ACLs beyond that case,
packaged launchers and real Minecraft still need manual validation on Windows.
