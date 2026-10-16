# onlyfile

onlyfile finds duplicate files in a folder and lets you move the extra copies to the trash, from a
terminal interface. It is for anyone who wants their disk space back without trusting a tool blindly:
nothing is removed before a byte-for-byte check, and every removal is logged.

It runs on macOS, Linux and Windows.

## Install

Prerequisite: Rust 1.88 or newer (`rustup update stable`). Nothing else: no system library.

```sh
cargo install --path apps/cli
```

This builds the `onlyfile` binary into `~/.cargo/bin`.

## Use

```sh
onlyfile                  # scan the current folder
onlyfile ~/Photos         # scan another folder
onlyfile --permanent ~/Downloads   # delete instead of moving to the trash
```

1. **Scan.** onlyfile walks the folder, then compares candidates, with a live count and a gauge. `q`
   cancels.
2. **Browse.** Groups of identical files are listed by the space their removal would free. The right
   pane shows every copy of the selected group with its modified date.
3. **Mark.** `a` marks every duplicate at once, keeping the oldest copy of each group (on equal
   dates, the shortest path); groups you already marked by hand are left as they are. `k` keeps the
   highlighted copy and marks the others; `space` marks or unmarks one copy; `u` unmarks everything.
   A group always keeps at least one unmarked copy.
4. **Remove.** `d` asks first, naming how many copies, how much space and where they go. `y` removes
   them; `n` cancels.
5. **Read the summary.** It lists what was removed and its size, every copy left in place or that
   could not be removed with the reason, and where the log is. Copies moved to the trash free their
   space once the trash is emptied.

`ctrl-c` during a removal stops it after the copy it is handling, so every removal is in the log;
the copies it did not reach stay marked for the next `d`. A second `ctrl-c` quits at once.

### Keys

| Key | Action |
|---|---|
| `↑` `↓` | Move in the focused list |
| `tab` | Switch between the groups and the copies |
| `a` | Mark all but the oldest copy of each group not marked by hand yet |
| `u` | Unmark every copy |
| `space` | Mark or unmark the highlighted copy |
| `k` | Keep the highlighted copy, mark the rest |
| `d` | Remove the marked copies, after asking |
| `s` | List the entries the scan skipped, with the reason |
| `enter` | Close the removal summary |
| `?` | Show or hide the help |
| `q`, `esc` | Quit (`esc` closes a list or a dialog first) |
| `ctrl-c` | Quit; during a removal, stop after the current copy (twice: quit at once) |

### What counts as a duplicate

Files are compared in four steps, each one only on the files that passed the one before:

1. Same size.
2. Same first, middle and last KiB.
3. Same xxh3-128 hash of the whole content.
4. Right before a removal, the same bytes as the copy the group keeps, read again from disk, and a
   different file from it. A copy that no longer matches, or that has become the kept file under
   another name (a folder replaced by a symlink, say), stays where it is.

Left out of the comparison, and listed under `s`:

| Entry | Why |
|---|---|
| Symlinks | Never followed, so a link can neither loop nor lead outside the folder |
| Special files (sockets, pipes, devices) | No content to compare |
| Empty files | Nothing to free, and often a placeholder something needs |
| Hardlinks | Another name for a file already counted: removing it frees nothing |
| Unreadable entries | Listed with the system's reason; the scan goes on without them |

### Where removed copies go

| Platform | Trash | `--permanent` |
|---|---|---|
| macOS | The Trash, through NSFileManager | Deleted |
| Linux | The freedesktop trash: `~/.local/share/Trash`, or the drive's own `.Trash-<uid>` for another drive (copied to the home trash when that folder cannot be created) | Deleted |
| Windows | The Recycle Bin | Deleted |

A copy the trash refuses is reported in the summary and stays where it is: onlyfile never deletes it
instead.

### Known limits

- **Windows, drives without a Recycle Bin** (some network shares and removable drives): Windows may
  delete a file it cannot keep instead of refusing it, and the trash library has no way to tell.
  Use `--permanent` knowingly there, or check the Recycle Bin first.
- **Windows, ReFS and Dev Drive volumes**: files are told apart by a 64-bit index that ReFS does not
  guarantee unique, so two different files there can be taken for hardlinks of each other. The
  effect is missed duplicates, never a wrong removal: the same check refuses to remove a copy it
  takes for the kept file.

### The log

Every run appends to one file. Each removed copy gets a line with its path, size and mode (`trash` or
`permanent`), and each copy left in place gets a warning with the reason.

| Platform | Log file |
|---|---|
| macOS | `~/Library/Application Support/onlyfile/onlyfile.log` |
| Linux | `$XDG_DATA_HOME/onlyfile/onlyfile.log`, by default `~/.local/share/onlyfile/onlyfile.log` |
| Windows | `%LOCALAPPDATA%\onlyfile\onlyfile.log` |

### Exit codes

| Code | Meaning |
|---|---|
| 0 | Quit normally, including a cancelled scan |
| 1 | Could not run: the folder is missing, unreadable or a file; the output is not a terminal; the log file or the terminal failed; the scan or the removal stopped |
| 2 | Wrong arguments (see `onlyfile --help`) |

## Develop

[`just`](https://github.com/casey/just) runs everything CI runs:

| Command | What it does |
|---|---|
| `just gate` | `fmt-check`, `lint` and `test`, in CI's order. Run it before pushing. |
| `just test` | `cargo test --workspace` |
| `just lint` | `cargo clippy --workspace --all-targets -- -D warnings` |
| `just fmt` | Format the workspace |
| `just run ~/Photos` | Run the release build on a folder |

One test really moves a temporary file to the trash, checks it arrived, then removes it from the
trash. On macOS it reads `~/.Trash`, which may need the terminal to have Full Disk Access; without it,
the test fails and says so.

CI runs `just gate` on Linux, macOS and Windows for every push to `main` and every pull request, and
checks that the workspace still builds with Rust 1.88.

### Layout

| Path | Owns |
|---|---|
| `libs/model` | The shared types: scanned files, duplicate groups, skipped entries |
| `libs/scanner` | Walking a folder into the files worth comparing, and the entries left out |
| `libs/replica` | Grouping files with identical content, hardlinks counted once |
| `libs/identity` | Telling whether two paths or open files are one file on disk |
| `libs/purge` | Checking each marked copy against the kept one, then trashing or deleting it |
| `apps/cli` | The `onlyfile` binary: arguments, log file, worker threads and screens |

The libraries do no input or output beyond the files they read or remove, and know nothing of the
terminal; the binary depends on them, never the other way round.

### Contributing

- Branches follow the change type: `feat/...`, `fix/...`, `refactor/...`, `docs/...`.
- Commits follow [Conventional Commits](https://www.conventionalcommits.org): `feat(scanner): ...`,
  one concern each.
- `just gate` passes before a pull request.

## License

MIT, see [LICENSE](LICENSE).
