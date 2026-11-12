# The desktop

!!! warning "Experimental"
    The desktop is on TorqOS's main branch and in the `0.4.0` release candidate that is in
    preparation. It is not in any released version yet. It has run on exactly one physical machine,
    the owner's ASUS Zenbook S 16, and otherwise only in virtual machines used for testing. Expect
    rough edges.

The TorqOS base has no desktop and no graphical stack. The desktop is a configuration you can turn
on: [COSMIC](https://system76.com/cosmic) (System76's desktop), and only COSMIC, with its login
screen, plus three TorqOS pieces described below. It is off by default.

## Turn it on

In configuration mode:

```text
alice@torq> configure
alice@torq# set torq desktop enable true
alice@torq# show | compare
alice@torq# commit
```

This is a system change, so it asks an administrator's password. Like any other change it is
recorded in the ledger and can be undone with `rollback` or `request system revert`. The machine
takes COSMIC from NixOS's binary cache; expect a large download. To turn it off again, set the value
to `false` (or `delete torq desktop enable`) and commit.

A change that makes COSMIC itself compile on your machine is a separate, off-by-default choice
(`set torq desktop customCompile true`), needed only for TorqOS's own touchpad gestures
(`set torq desktop touchpadGestures true`). Compiling COSMIC's compositor takes a long time on a
laptop; leave both off unless you want them.

## What you get

**TorqOS Settings** is `torqcli` in a COSMIC window, in the application menu. It shows the
configuration tree, your candidate, commits, the history, rollback, changes waiting for you to
approve, alarms and backups. It talks to the same broker as the command line and offers only
things the command line can do, so anything you do there is the same change as the matching
`torqcli` command, in the same ledger.

**The TorqOS panel applet** sits in COSMIC's panel by default. It shows the machine's state at a
glance: a confirmed commit waiting for you, a log-out or restart that a switch needs, an alarm,
proposals waiting, an uncommitted draft. It changes nothing by itself; it points you to TorqOS
Settings, or confirms a commit when you ask.

**The SSH tunnels applet** is in COSMIC's list of applets, not in the default panel (add it in
COSMIC's panel settings). It starts, stops and adds your own SSH tunnels. See
[SSH tunnels](ssh-tunnels.md).

**Approval is a dialog.** On a desktop, a system change asks for an administrator's password in a
COSMIC dialog (through polkit, the system's permission service) instead of at the terminal. The
dialog shows the broker's own description of the change. No dialog is a trusted path: a program
that can draw a window could imitate it. The password is refused if it is sent to the broker from
the desktop directly, so a copy taken by an imitation cannot be replayed there (KNOWN-GAPS 6zw in
the repository).

## Your desktop settings are saved

COSMIC keeps your panel, theme, wallpaper and shortcut settings as many small files. TorqOS saves
them in your own part of the configuration (`users/<you>/desktop.json`), so a machine rebuilt from
its repository looks the same, and an undo or a pull puts your desktop back as it was. Two undos
are refused: one that would put other programs' settings back (the save that took them out), and,
while your saved desktop still holds settings an earlier TorqOS kept, any undo of a desktop change.
For the second, `request system desktop save` first, then undo.

- By default this happens automatically, checked about every 15 minutes while the panel applet runs.
  Each save is a commit in your configuration and appears in the history as `Desktop: …`.
- `request system desktop saving manual` stops that; `request system desktop save` then saves when
  you ask. `request system desktop saving automatic` turns it back on. `show system desktop` says
  what is saved and what is not yet.
- Only COSMIC's own settings are saved. Applets and programs from anyone else that keep their
  settings in COSMIC's folder (a weather applet's location, a token) are left out, and TorqOS
  refuses them if anything tries to save them. Their settings stay where they are, in your home.
- Settings that belong to the machine rather than to you (which display the panel is on, when the
  machine sleeps) are left out, and so is what COSMIC writes by itself (the screenshot tool's last
  area). A save happens only when you changed something.

!!! warning "What is saved is readable by every account"
    The configuration repository can be read by every account on the machine, and by a remote if you
    set one up. COSMIC's own settings can say something about you: your wallpaper's path, your custom
    shortcuts (the commands you typed), the panel's applets and the dock's favourites (what is
    installed), and, when you have set them, Files' favourites (network places), the terminal's
    profiles (commands and hosts), wallpaper folders and window rules. They are saved, as your
    desktop. If that matters, set saving to manual and save when you choose.

!!! warning "Saved before 0.4.0-rc1: other programs' settings stay in the history"
    Before this release, the save also took other applets' settings. The next save takes
    them out of `desktop.json` (with saving manual, run `request system desktop save` once), but the
    earlier commits still hold them, on this machine and on a
    remote that received them. If that matters (a location, a token), change what you can, such as
    the token, and see KNOWN-GAPS 6zy in the repository for what can be done about the history.

## Limits

- **One machine so far.** Other hardware may behave differently.
- **An applet that exits stays gone** until the panel restarts (or you log in again). If the TorqOS
  applet crashes, TorqOS is missing from the panel, and the approval dialog's automatic restart
  depends on the applet running, until then. Log out and in again to bring it back. (A terminal's
  commands still start the dialog themselves.)
- **The lid.** Some laptop panels cannot wake from suspend (the owner's OLED display is one). For
  those, `set torq desktop dimInsteadOfSuspend true` makes the machine never suspend: closing the lid
  locks it and turns the display fully down, and opening it brings the brightness back. It is off by
  default; with it on, a closed lid does not save battery as suspend would.
- **COSMIC's own settings pages** are not yet wrapped: a page that changes something TorqOS owns
  (the time zone, the host name, users) does not hand the change to TorqOS as a proposal yet. Use
  `torqcli` or TorqOS Settings for those.
