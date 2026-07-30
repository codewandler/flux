# portable-parity.flux — the C-271 parity program.
#
# Deliberately trivial and deliberately MODEL-FREE: not one node here is a `call`, so the flow
# needs no operation catalog, no tool, no provider, no credential and no clock. It exercises the
# pure fragment of Flux-Lang only — literals, an operator formula (`expr`), a conditional, string
# interpolation (`fmt`) and a return.
#
# The deliverable this proves is the SUBSTRATE, not the program. A larger example would make a
# mismatch ambiguous between "the port is wrong" and "the program is wrong".

flow portable_parity
  $base = 6
  $factor = 7
  $answer = $base * $factor
  $big = $answer > 40
  when $big
    $verdict = "big"
  else
    $verdict = "small"
  $label = fmt("flux-portable answer={answer} verdict={verdict}")
  return $label
