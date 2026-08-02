# Networking interoperability checks

Automated loopback coverage lives beside the networking implementation:

- `irc::stream::tests::socks4a_connect_uses_the_configured_proxy_and_user_id`
  verifies the SOCKS4a wire request, user ID, hostname and success reply;
- `irc::stream::tests::direct_connection_binds_the_selected_local_address`
  verifies the selected source address on both ends of a TCP connection;
- `irc::dcc::tests::listeners_are_nonblocking_before_tokio_adopts_them` and
  `dcc_configuration_normalizes_ranges_and_honours_bind_address` verify real
  listening sockets, port selection and interface binding;
- the DCC parser/formatter tests cover standard and mIRC passive CHAT, SEND,
  RESUME and ACCEPT tokens.

## External client matrix

Run this matrix before publishing a networking release. It requires two real
clients on mutually reachable machines; it cannot be simulated by parser tests.

| Peer | Standard CHAT | Passive CHAT | Standard SEND/resume | Passive SEND/resume | DCC Server |
| --- | --- | --- | --- | --- | --- |
| mIRC (current) | pending | pending | pending | pending | pending |
| HexChat or KVIrc | pending | pending | pending | pending | n/a |

For each listener test, set a narrow DCC port range, select the intended local
bind address, confirm the listener uses that range/interface, transfer a file
larger than 4 MiB, interrupt it, and verify resume plus final byte equality.

No compatible peer client is installed in the current development environment,
so external rows must remain marked pending until they are actually exercised.

## identd decision

identd is intentionally deferred. Modern IRC networks do not normally require
clients to expose TCP port 113, and adding a privileged inbound service increases
firewall and privacy surface. Reconsider it only when a reproducible connection
failure on a supported older network demonstrates a real user requirement.
