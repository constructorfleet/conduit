# Filesystem-Imported Models Are Read-Only In The UI

Excita accepts wake-word models from two sources: `POST /models/import`
(operator-uploaded through the UI) and a bind-mounted `models/` directory
that Excita scans on boot and on `SIGHUP`. Models from either source are
equally deployable. **They differ in one place: a filesystem-imported
model cannot be deleted through the UI.** Retiring one means removing the
file on disk. UI-uploaded models remain fully mutable from the UI.

**Why keep both import paths**

Filesystem drop and HTTP upload solve different problems and neither
subsumes the other:

- Filesystem drop is how a deployment bootstraps. A first-boot Excita
  behind Compose has no operator, no browser, and no models — a
  bind-mounted `models/` directory is the answer that works before the
  UI does. It's also the shape sysadmins already know from every other
  service in the stack.
- HTTP upload is how an operator brings a model *they trained
  elsewhere* into a running Excita and expects Excita to own its metadata
  going forward. Making them shell into the host and touch a directory
  would be a regression from the openWakeWord train-in-place experience.

**Why the UI cannot delete filesystem-imported models**

The alternative is: UI deletes the row, the next scan re-imports the
file, and the operator asks "why did my model come back after I deleted
it?" This gets asked exactly once per operator, and there is no answer
that satisfies them. The choices are:

1. UI-delete removes the file on disk. Excita is a service that
   silently rewrites bind-mounted host directories, which is a rule
   surprise the operator did not consent to when they mounted the
   volume.
2. UI-delete blocks the scanner from re-importing (persist a
   tombstone). The file on disk is now a lie — present but hidden —
   and the tombstone lives in Excita's DB, not next to the file, so
   moving the deployment loses the "deletion" and the model comes
   back anyway.
3. UI-delete simply doesn't apply to filesystem-imported models.
   The file on disk is authoritative; removing it is how the
   operator retires the model. The UI shows the "delete" control as
   disabled with a tooltip pointing at the directory.

Option 3 is the only one where the mental model matches the file
system's own model: the volume is the source of truth, Excita reflects
it.

The `model` row still carries a soft-delete `deleted_at` column (per
spec 0011), so an *upload*-sourced model behaves as spec described. A
filesystem-sourced row's `deleted_at` is only set as part of a
subsequent scan noticing the file is gone.

**Consequences**

- The `model` row grows a `source` column (`upload` | `filesystem`)
  plus, for filesystem rows, the path relative to the mount root.
- The scanner reconciles: new files → insert, missing files →
  soft-delete, changed files (mtime + size, not content hash by
  default) → new model version.
- The UI grays out "delete" for `source=filesystem` and links to the
  mount path so the operator knows where to go.
- Uploaded model artifacts live in Excita's own storage
  (`EXCITA_DATA_DIR/models/`), which is *not* the same directory as
  the import mount, so an upload cannot silently overwrite an
  imported file and vice versa.
