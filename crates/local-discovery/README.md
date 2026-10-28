# Windows local discovery: experimental reader

This crate is an isolated building block for future automatic local mod pairing.
It is not connected to the desktop application or the mod and does not make
pairing automatic. The descriptor is a bearer capability and must never be
logged, shown in the interface or passed on a command line.

On Windows the reader opens a file without following its final reparse point,
checks its owner and a protected, owner-only DACL on the open handle, and reads
at most 4097 bytes. It fails closed for other ACLs. This protects against other
Windows users, not software already running as the same user or an administrator.
Parsing alone does not check expiry, process identity, file provenance, or
whether an endpoint really belongs to Minecraft.

Before integration, the mod needs a writer that creates the file privately
before writing any secret, plus expiry, lifecycle and multi-instance handling.
The desktop must authenticate the bridge and bind it to the chosen game session.
The whole flow must be tested on Windows, including stale files and hostile ACLs.
