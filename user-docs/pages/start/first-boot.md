# First-boot setup

The first time a TorqOS image boots, the console asks a few questions, once, like a network device's
initial configuration dialog.

## The questions

1. **The machine's name** and **time zone**.
2. **Your user name and password** (asked twice). This is the machine's owner account.
3. **How to log in from other machines:**
    - **not at all**: the default; pressing ++enter++ turns nothing on;
    - **by password**;
    - **by SSH key**: paste your public key;
    - **both**.

## What happens next

Setup writes the machine's configuration from your answers, and keeps your password in a file only
the system can read, never in the configuration. TorqOS then builds that configuration, switches to
it and records it as **change #1**, "TorqOS set up".

If the configuration does not build, nothing changes and setup asks again. If setup is interrupted
(by a power cut, say), it picks up where it left off at the next boot.

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
