# TorqOS's prompt (modules/shell.nix): who and where, the git branch, and TorqOS's own state —
#
#   alice@host ~/torq  ⎇ main*  ● candidate
#   ❯                                                  21:07
#
# Plain Unicode only: it looks right in any terminal, without a patched font. It follows the
# window: a long folder is cut from the left (…/deep/folder) to a third of its width, and zsh shows
# the time on the right only when there is room. The arrow is green after a command that worked,
# red after one that failed.

# The machine's own text console (TERM=linux) has a small font without ⎇ ● ⚠ ❯ …: plain characters
# there; any other terminal (over SSH) gets the real ones.
if [[ $TERM == linux ]]; then
  _torq_branch="git:" _torq_candidate="+" _torq_warn="!" _torq_arrow=">" _torq_more="..."
else
  _torq_branch="⎇ " _torq_candidate="●" _torq_warn="⚠" _torq_arrow="❯" _torq_more="…"
fi

# TorqOS's state, at a glance: the machine not as its history says (the drift check, every 15
# minutes, leaves /run/torq/drift), and a candidate of yours waiting (configure … without commit).
torq_prompt_state() {
  if [[ -e /run/torq/drift ]]; then
    print -n "%F{red}${_torq_warn} not as its history says%f "
  fi
  if [[ -s ${XDG_STATE_HOME:-$HOME/.local/state}/torq/candidate.json ]]; then
    print -n "%F{yellow}${_torq_candidate} candidate%f "
  fi
}

ZSH_THEME_GIT_PROMPT_PREFIX="%F{magenta}${_torq_branch}"
ZSH_THEME_GIT_PROMPT_SUFFIX="%f "
ZSH_THEME_GIT_PROMPT_DIRTY="%F{yellow}*"
ZSH_THEME_GIT_PROMPT_CLEAN=""

PROMPT='%F{cyan}%n%f%F{8}@%f%F{blue}%m%f %B%F{white}%$((COLUMNS / 3))<${_torq_more}<%~%<<%f%b $(git_prompt_info)$(torq_prompt_state)
%(?.%F{green}.%F{red})${_torq_arrow}%f '
RPROMPT='%F{8}%*%f'
