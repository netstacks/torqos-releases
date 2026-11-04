# TorqOS's prompt (modules/shell.nix): who and where, the git branch, and TorqOS's own state —
#
#   alice@host ~/torq  ⎇ main*  ● candidate
#   ❯                                                  21:07
#
# Plain Unicode only: it looks right in any terminal, without a patched font. It follows the
# window: a long folder is cut from the left (…/deep/folder) to a third of its width, and zsh shows
# the time on the right only when there is room. The arrow is green after a command that worked,
# red after one that failed.
#
# In TorqOS's colours (branding/README.md, the palette; torqos.org's terminal): who you are and
# what is waiting for you in Torque, the folder in Foreground, the rest in Steel and Slate, "worked"
# in Signal green. Exact in a terminal that says it shows 24-bit colour (COLORTERM), the nearest of
# 256 colours elsewhere, and the console's own few colours on its text console.

# The machine's own text console (TERM=linux) has a small font without ⎇ ● ⚠ ❯ …: plain characters
# there; any other terminal (over SSH) gets the real ones.
if [[ $TERM == linux ]]; then
  _torq_branch="git:" _torq_candidate="+" _torq_warn="!" _torq_arrow=">" _torq_more="..."
else
  _torq_branch="⎇ " _torq_candidate="●" _torq_warn="⚠" _torq_arrow="❯" _torq_more="…"
fi

typeset -gA _torq_c
if [[ $TERM == linux ]]; then
  # The VGA palette's brown is its orange; it has no grey but white (zsh drops colour 8 there).
  _torq_c=(torque yellow fg white steel white slate white green green alarm red)
elif [[ $COLORTERM == (truecolor|24bit) ]]; then
  _torq_c=(torque '#FF7A1A' fg '#ECEDEF' steel '#8A93A3' slate '#5B6270' green '#7EE2A8' alarm '#FF5F5F')
else
  _torq_c=(torque 208 fg 255 steel 246 slate 241 green 115 alarm 203)
fi

# TorqOS's state, at a glance: the machine not as its history says (the drift check, every 15
# minutes, leaves /run/torq/drift), and a candidate of yours waiting (configure … without commit).
torq_prompt_state() {
  if [[ -e /run/torq/drift ]]; then
    print -n "%F{${_torq_c[alarm]}}${_torq_warn} not as its history says%f "
  fi
  if [[ -s ${XDG_STATE_HOME:-$HOME/.local/state}/torq/candidate.json ]]; then
    print -n "%F{${_torq_c[torque]}}${_torq_candidate} candidate%f "
  fi
}

ZSH_THEME_GIT_PROMPT_PREFIX="%F{${_torq_c[steel]}}${_torq_branch}"
ZSH_THEME_GIT_PROMPT_SUFFIX="%f "
ZSH_THEME_GIT_PROMPT_DIRTY="%F{${_torq_c[torque]}}*"
ZSH_THEME_GIT_PROMPT_CLEAN=""

PROMPT='%F{${_torq_c[torque]}}%n%f%F{${_torq_c[slate]}}@%f%F{${_torq_c[torque]}}%m%f %B%F{${_torq_c[fg]}}%$((COLUMNS / 3))<${_torq_more}<%~%<<%f%b $(git_prompt_info)$(torq_prompt_state)
%(?.%F{${_torq_c[green]}}.%F{${_torq_c[alarm]}})${_torq_arrow}%f '
RPROMPT='%F{${_torq_c[slate]}}%*%f'
