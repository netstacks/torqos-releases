# First-boot setup

The first time a TorqOS image boots, the console asks a few questions, once, like a network device's
initial configuration dialog.

## The questions

1. **The network.** If the machine is already online (a cable), setup says so and goes on. If not,
   it lists the Wi-Fi networks it can see: choose one by number and give its passphrase, press
   `r` to look again, or `s` to skip and continue without a network. Without one, TorqOS still
   installs.
2. **A new machine, or from a repository.** Only asked when the machine is online. "A new machine"
   continues below. "From a repository" means that you give the SSH address of a TorqOS
   configuration repository of your own, add the key setup shows you to that repository as a deploy
   key with write access, check what setup found in it (its ledger, its users, the host's key), set
   the owner's password for this machine, and confirm. The machine then becomes the machine that
   repository describes: its history goes on here, with this machine's own hardware and software.
   Be careful: that configuration will run on this machine as root. Offline, a new machine is the
   only choice. See [The remote](../guide/remote.md).
3. **The machine's name** and **time zone**.
4. **Your user name and password** (asked twice). This is the machine's owner account.
5. **How to log in from other machines:**
    - **not at all**: the default; pressing ++enter++ turns nothing on;
    - **by password**;
    - **by SSH key**: paste your public key;
    - **both**.

## What happens next

Setup writes the machine's configuration from your answers, and keeps your password in a file only
the system can read, never in the configuration. TorqOS then builds that configuration, switches to
it and records it as **change #1**, "TorqOS set up".

If the configuration does not build, nothing changes and setup starts over from its questions. If
setup is interrupted before the switch (by a power cut, say), it starts again from the beginning at
the next boot, with its partial work removed. If it is interrupted after the switch, the machine
starts its new system and finishes recording change #1 by itself.

On real hardware, the machine restarts into the new system when setup is done.

When it is done, the console shows a login prompt. Log in with the user name and password you chose,
then run `torqcli` to manage the machine. Setup never runs again.

## Where remote login stands

| You chose | SSH is |
|---|---|
| Not at all | off; nothing listens on the network |
| By SSH key | on, keys only, for you only |
| By password | on, passwords allowed, for you only; repeated wrong guesses are blocked |
| Both | on, keys and passwords, for you only |

`root` never logs in over SSH, whatever you chose. You can change any of this later in the
configuration; see [Users, passwords and SSH](../guide/users-ssh.md).

Next: [Your first session](first-session.md).
