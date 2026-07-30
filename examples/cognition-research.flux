flow cognition-research -> Answer
  need: Need = need({"ask":"what changed in enterprise pricing since 2026-01-01?","require":["date","plan","price","source"]})
  src = grep("enterprise pricing")
  ranked = sort({"by":"confidence","items":[],"order":"desc"})
  claims = top({"items":[],"n":8})
  ctx pack
    purpose "the evidence backing the pricing answer"
    budget 6000
    include src, claims
  open = gaps({"claims":[],"need":{}})
  repeat 2, until: open
    more = grep("pricing change")
    pack += more
    open = gaps({"claims":[],"need":{}})
  cited = cite({"claims":[]})
  return cited
