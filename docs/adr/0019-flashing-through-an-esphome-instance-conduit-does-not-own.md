# Flashing Through An ESPHome Instance Conduit Does Not Own

Conduit gains a relationship with an ESPHome instance the operator already runs,
and delegates compilation and flashing to it. The console offers a flash
affordance that hands off to that instance rather than serving a binary Conduit
built. Conduit never compiles firmware, never stores a compiled image, and never
serves an artifact containing a device token.

This answers question 4 of #122, which
[ADR-0015](0015-render-the-conduit-part-of-the-firmware.md) deferred with "not
yet, and on purpose". ADR-0015's fragment decision is unchanged and load-bearing
here: the fragment is what makes this possible without Conduit modelling
hardware.

## What a flash button actually needs, which is not YAML

The obvious version of this feature is an `<esp-web-install-button>` on a
device's page. Reading what that element requires is what settles the design,
because it needs none of what Conduit renders.

ESP Web Tools flashes over WebSerial from a **manifest plus a compiled `.bin`
per chip family**. It cannot consume ESPHome YAML; nothing in the browser can.
So a page that flashes requires an answer to "who ran `esphome compile`", and
Conduit has no toolchain — no Python, no PlatformIO, no multi-gigabyte framework
cache. Rendering YAML and flashing a device are not two views of one feature.
They are separated by a build.

## The reason not to build that toolchain is a credential, not effort

ESPHome substitutes `secrets.yaml` values into generated C++ at compile time.
The device token is therefore **inside** the binary, as a string in flash.

That inverts ADR-0015's entire secrets posture. The point of emitting
`token: !secret conduit_token` is that the rendered artifact is safe to commit
and useless to whoever holds it. A compiled image is the opposite: it is a file
whose whole purpose is to be downloaded, and it carries a working credential for
the household's voice assistant. The moment Conduit serves something flashable,
it is serving a secret, and:

- it needs a policy for who may download one, distinct from who may render YAML
- it needs to decide whether images are cached on disk, which is a token at rest
  in a new place
- it must not serve one over plain HTTP — WebSerial requires a secure context
  anyway, so the browser enforces half of this, but Conduit's own listener does
  not have to be HTTPS today and `README.md`'s deployment posture assumes it
  often is not

None of that is unsolvable. All of it is a second security model beside the one
ADR-0015 just established, maintained by the same people, and the first
disagreement between the two is a leaked token. Declining to hold the artifact is
how the posture stays single.

## Decision: delegate to an instance the operator points us at

An operator configures Conduit with the base URL of an ESPHome dashboard they
already run. Conduit posts the assembled configuration there and links to that
dashboard's own install and OTA affordances.

Three properties make this the right shape rather than merely the cheap one:

**The toolchain already exists and is already trusted with these secrets.** An
operator flashing a Conduit satellite today has an ESPHome instance — it is how
both board files get compiled, and `firmware/README.md` documents the
`secrets.yaml` they keep there. The token is already at rest in that instance.
Delegating puts no credential anywhere it is not already.

**Conduit stores no image.** Compilation, caching, and the `.bin` stay on the
ESPHome side. The artifact Conduit produces is still the fragment: text,
`!secret`-bearing, safe to commit. ADR-0015's rule survives verbatim.

**It is the relationship ADR-0015 said was missing.** That ADR's consequences
list reads "rendered YAML still has to reach a device, and Conduit has no
relationship with an ESPHome instance". This creates exactly one, narrowly: a
configured base URL, an upload, and a link. Not a new build system.

### What "assembled" means, and why the fragment still wins

ESPHome compiles a *document*, not a fragment, so something has to combine the
hand-written board file with the rendered blocks. That happens at upload time:
Conduit sends the fragment to the ESPHome instance as its own file, and the board
file `!include`s it — which is precisely the arrangement
[spec 0003](../specs/0003-firmware-fragment-rendering.md) track C already
establishes for the checked-in boards.

So the board file is uploaded once by hand, or lives in the ESPHome config
directory already, and **only the fragment is ever re-uploaded**. Reconfiguring a
device is a one-file write plus a build the operator triggers. Conduit still
never sees `cs_pin`, `i2s_mclk_pin`, or the PD-negotiation script, and a board
Conduit has never heard of still works. The whole-document alternative was
rejected in ADR-0015 decision one and nothing here revisits it — if anything the
flash path depends on that rejection, because it is what keeps the upload to one
small file.

## Consequences

- **The ESPHome base URL is new configuration**, and it is a URL Conduit posts
  to. It is therefore an SSRF surface: an operator-supplied address that the
  server dials. It gets the treatment such a field needs — validated scheme,
  and the failure to reach it reported as a failure rather than retried into a
  scan.
- **ESPHome's API is not a stable contract.** Its dashboard endpoints are not
  versioned for third parties and can change between releases. This coupling is
  weaker than the renderer/component coupling of ADR-0015 decision five, but it
  is real, and a broken upload must degrade to "here is your fragment, apply it
  yourself" rather than to a dead page. **The download affordance from spec 0003
  is therefore not superseded by this ADR — it is the fallback, and it stays.**
- **First adoption is not solved here.** A device with no firmware at all cannot
  receive OTA, and ESPHome's own install flow is where that begins. Conduit links
  to it rather than reimplementing it.
- Conduit gains no dependency on a Python toolchain, no container to bundle, and
  no build cache to manage. This is the cost avoided, and it is the largest one.
- A flash triggered from Conduit's console is an action with a physical effect on
  a device, which is a different class of thing from every other management
  route. It is an operator-initiated hand-off to a tool that asks for its own
  confirmation, not a fire-and-forget POST.

## Alternatives rejected

**Bundle an `esphome` container and compile in-process.** Self-contained, and
the operator points at nothing. It also makes Conduit responsible for a
PlatformIO toolchain, multi-gigabyte framework caches, first builds measured in
minutes, and — decisively — for storing and serving binaries with a device token
compiled into them. Rejected on the credential, not the disk space.

**Serve a prebuilt generic factory image for adoption, then OTA for everything
after.** Genuinely attractive: the image carries no device token, so the
security objection disappears, and it is the smallest possible flash button.
Rejected for now because a generic image still needs a per-board build Conduit
does not perform, and because it splits configuration across two mechanisms —
one for the first flash and one for every change after. Worth revisiting if
delegation proves awkward; it is not foreclosed by this decision.

**Serve YAML only, with a copy button.** What spec 0003 already specifies, and
what this ADR keeps as the fallback path. Rejected as the *whole* answer because
configuration that still requires someone to hand-carry a file to a build has
not fully moved, which is the same reasoning that rejected a CLI renderer in
ADR-0015 decision three.
