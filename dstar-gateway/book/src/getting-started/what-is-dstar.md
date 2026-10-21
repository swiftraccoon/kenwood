# Reflector protocol boundaries

This stack implements the DPlus, DExtra and DCS network protocols between a
gateway client and reflector server. Radio-to-modem control belongs to the
separate model and modem libraries.

## Reflectors and modules

A reflector groups linked clients by module. This stack represents module
selectors as A through Z; each reflector configures which selectors it serves.
A configured module's availability is a server policy, not a radio limit.

| Protocol | Conventional directory prefix | Default UDP port |
| --- | --- | --- |
| DPlus | REF | 20001 |
| DExtra | XRF; XLX directories also expose DExtra endpoints | 30001 |
| DCS | DCS | 30051 |

These are the pinned reference implementations' defaults. A client uses its
configured address and port. Selecting an XLX entry does not imply that every
protocol revision or server extension is implemented.

## Linking and completion

DPlus attaches a TCP-authenticated host list before its two-round UDP
handshake. DExtra and DCS use one-round UDP handshakes. DCS carries the target
reflector identity in LINK, POLL and UNLINK, so its builder requires that value.
The core correlates replies with the applicable identity, modules and phase.

While linked, the client exchanges protocol-specific keepalives and receives
voice events. UNLINK acknowledgement differs by protocol/revision. The
revision-zero DExtra request used here can close without a reply; a client
waiting for acknowledgement then reports its protocol timeout.

AsyncSession::disconnect waits for the terminal outcome and task completion.
Dropping the handle requests local exit without a remote acknowledgement.
The caller keeps consuming events while linked so receive/timer work can
progress through the bounded event queue.

## Voice and slow data

A voice frame carries nine AMBE bytes and three slow-data bytes for a
20-ms interval, or 50 frames/s. DExtra sends a 56-byte voice header and
27-byte data/EOT packets. DPlus uses 58-byte headers, 29-byte data and
32-byte EOT packets. DCS embeds the header in each 100-byte voice packet.

A nonzero 16-bit StreamId identifies a transmission. Transmit indices use
FrameSequence values 0 through 20; the codec adds termination separately.
Receive events retain the complete sequence byte, including unknown bits.
Slow-data blocks span paired voice frames, and DPRS sentences span multiple
GPS blocks. The bounded listener example waits for CR and verifies the checksum.

Source routines and exact revisions are listed in
[REFERENCES](https://github.com/swiftraccoon/kenwood/blob/main/dstar-gateway/REFERENCES.md),
including ircDDBGateway Common/DStarDefines.h, ConnectData.cpp and
AMBEData.cpp, and xlxd cdextraprotocol.cpp/cdcsprotocol.cpp.
