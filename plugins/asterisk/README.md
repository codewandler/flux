# Asterisk plugin

The Asterisk plugin exposes two interfaces through Flux's guarded plugin host:

- eight established Asterisk Manager Interface (AMI) operations over a host-owned TCP connection;
- the complete Asterisk REST Interface (ARI) 22.10.1 contract: 108 request/response operations and
  the event WebSocket, plus two Flux-owned WebSocket lifecycle controls.

The plugin performs no network IO directly. Flux resolves configuration, injects credentials and
applies private-network policy before opening the AMI connection, making an ARI HTTP request, or
opening the ARI WebSocket.

## Build and install locally

From the repository root:

```bash
cargo build --release --manifest-path plugins/Cargo.toml -p asterisk
flux plugin add asterisk plugins/target/release/flux-plugin-asterisk
flux plugin skill --install
```

The last command refreshes the generated plugin skill after the manifest changes. A signed plugin-
pack installation uses `flux plugin install asterisk` instead of `plugin add`.

## AMI setup

Configure an AMI account in Asterisk, then set:

```bash
export ASTERISK_AMI_USERNAME='flux'
export ASTERISK_AMI_SECRET='...'
export ASTERISK_AMI_HOST='localhost' # optional; defaults to localhost
export ASTERISK_AMI_PORT='5038'      # optional; defaults to 5038
```

The host resolves the username and secret by purpose and opens the guarded TCP connection. The
plugin does not read them from its process environment. The existing operation identities remain:

```text
asterisk.ami.ping
asterisk.channel.list
asterisk.peer.list
asterisk.queue.status
asterisk.devicestate.list
asterisk.channel.hangup
asterisk.call.originate
asterisk.command
```

Start with the read-only probe:

```bash
flux plugin call asterisk asterisk.ami.ping '{}'
```

## ARI setup

Enable Asterisk's HTTP server and ARI user, then set the matching Basic-auth credentials:

```bash
export ASTERISK_ARI_USERNAME='flux'
export ASTERISK_ARI_PASSWORD='...'
# Optional; this is the host-side default:
export ASTERISK_ARI_URL='http://localhost:8088/ari/'
```

`ASTERISK_ARI_URL` is an endpoint reference resolved by Flux. The plugin receives neither the URL
nor the Basic-auth password. The host injects the username/password pair only after the operation is
authorized.

ARI normally resolves to loopback or a private PBX address. Grant only the host used by your
deployment in `.flux/config.toml`:

```toml
[private_net.plugins]
asterisk = ["localhost"] # replace with the PBX hostname when remote
```

The operator grant is intersected with the plugin manifest's own private-host declaration; neither
side grants access alone. Verify the REST path with the read-only ARI ping:

```bash
flux plugin call asterisk asterisk.ari.asterisk.ping '{}'
```

## ARI coverage and special responses

The manifest accounts for all 109 official Swagger operation facts. Of those, 108 use the generated
REST executor. It percent-encodes declared path/query values, emits only the declared JSON body,
preserves vendor JSON fields and reports non-2xx status/body details. Mutating and destructive live-
call operations retain their reviewed risk and semantic effects.

Two routes need explicit host-owned handling:

- `asterisk.ari.recordings.getStoredFile` streams the stored recording into Flux's bounded scratch
  blob store and returns only `blob_ref`, `size` and `sha256`; recording bytes do not cross the plugin
  protocol as inline base64.
- `asterisk.ari.events.eventWebsocket` accounts for the official `/events` Swagger fact and opens an
  authenticated, host-owned WebSocket. `asterisk.ari.control.events.read` performs one bounded read
  and `asterisk.ari.control.events.close` closes the session-scoped WebSocket id. Those last two are
  Flux lifecycle controls, not extra vendor operations.

WebSocket reads return a typed JSON event, timeout receipt, or close receipt. Binary frames are
refused because ARI events are JSON text; ping/pong and connection cleanup remain host-owned.

## Live smoke

The repository smoke script runs only when the matching credentials are already present. It never
prints or embeds them:

```bash
ASTERISK_ARI_USERNAME=flux \
ASTERISK_ARI_PASSWORD='...' \
ASTERISK_ARI_URL='http://pbx.internal:8088/ari/' \
scripts/smoke-plugins.sh
```

The ARI case calls only `asterisk.ari.asterisk.ping`. The script also installs the generated plugin
skill into its isolated temporary Flux home and checks that the Asterisk reference carries the AMI
and ARI setup, official event WebSocket and Flux lifecycle controls. It does not modify the
operator's plugin registry or configuration.

## Contract provenance

The development-only Swagger source, immutable Asterisk release/commit, byte hashes and deterministic
regeneration commands are documented in [specs/README.md](specs/README.md). The plugin binary embeds
only generated Rust contracts, not the vendored Swagger documents.
