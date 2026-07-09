//! The `expr` evaluator — a safe, side-effect-free formula mini-language for conditional logic,
//! arithmetic, string functions, and list operations. Used by `Node::Expr` in the runtime and by
//! transform operations (`filter`, `map`, etc.) for predicate evaluation. Public so `flux-tools`
//! can reuse the exact semantics for op-side predicates (L-46).

use crate::FlowError;
use crate::Result;
use std::collections::{BTreeMap, BTreeSet, VecDeque};

/// A value produced by the `expr` evaluator: number, string, bool, list, or object. Arithmetic,
/// comparison, boolean, and string functions all flow through this typed value, with lenient numeric
/// coercion for backward compatibility (a numeric string participates in arithmetic as the number it
/// spells). `List` (L-35) exists so `len(...)` on a bound array counts elements rather than falling
/// through to text-length on the array's stringified JSON; it does not participate in arithmetic.
/// `Obj` (L-46) preserves object structure for dotted access and key-count; objects were previously
/// stringified, so `len(obj_var)` returned character count — now it returns key count.
#[derive(Clone, Debug, PartialEq)]
pub enum ExprVal {
    Num(f64),
    Str(String),
    Bool(bool),
    List(Vec<serde_json::Value>),
    Obj(serde_json::Map<String, serde_json::Value>),
}

impl ExprVal {
    /// Coerce to a number where it makes sense: bools are 0/1, numeric strings parse. Returns `None`
    /// for non-numeric strings (and always for a list or object), so arithmetic on them is a clean
    /// error rather than a silent 0.
    pub fn as_num(&self) -> Option<f64> {
        match self {
            ExprVal::Num(n) => Some(*n),
            ExprVal::Bool(b) => Some(if *b { 1.0 } else { 0.0 }),
            ExprVal::Str(s) => s.trim().parse::<f64>().ok(),
            ExprVal::List(_) | ExprVal::Obj(_) => None,
        }
    }

    /// Render to canonical text — the form stored and printed (numbers via [`format_number`]; a
    /// list or object as its compact JSON, matching how op results already textify arrays/objects).
    pub fn as_text(&self) -> String {
        match self {
            ExprVal::Num(n) => format_number(*n),
            ExprVal::Str(s) => s.clone(),
            ExprVal::Bool(b) => b.to_string(),
            ExprVal::List(items) => serde_json::to_string(items).unwrap_or_default(),
            ExprVal::Obj(map) => serde_json::to_string(map).unwrap_or_default(),
        }
    }

    /// Truthiness, matching [`json_truthy`] for strings so `expr` conditions read consistently. A
    /// list or object is truthy iff non-empty (mirrors `json_truthy`'s empty-array/empty-object rule).
    pub fn truthy(&self) -> bool {
        match self {
            ExprVal::Num(n) => *n != 0.0,
            ExprVal::Bool(b) => *b,
            ExprVal::Str(s) => {
                let t = s.trim();
                !t.is_empty() && !t.eq_ignore_ascii_case("false") && t != "0"
            }
            ExprVal::List(items) => !items.is_empty(),
            ExprVal::Obj(map) => !map.is_empty(),
        }
    }

    pub fn from_json(v: &serde_json::Value) -> ExprVal {
        match v {
            serde_json::Value::Number(n) => ExprVal::Num(n.as_f64().unwrap_or(0.0)),
            serde_json::Value::Bool(b) => ExprVal::Bool(*b),
            serde_json::Value::String(s) => ExprVal::Str(s.clone()),
            serde_json::Value::Null => ExprVal::Str(String::new()),
            serde_json::Value::Array(items) => ExprVal::List(items.clone()),
            serde_json::Value::Object(map) => ExprVal::Obj(map.clone()),
        }
    }

    /// Descend into an object or JSON-parseable string by field name, returning the value at that
    /// field or `Str("")` if the field is missing or this value is not an object.
    pub fn get_field(&self, field: &str) -> ExprVal {
        match self {
            ExprVal::Obj(map) => map
                .get(field)
                .map(ExprVal::from_json)
                .unwrap_or_else(|| ExprVal::Str(String::new())),
            ExprVal::Str(s) => {
                // Try to parse as JSON object
                if let Ok(serde_json::Value::Object(map)) = serde_json::from_str(s) {
                    map.get(field)
                        .map(ExprVal::from_json)
                        .unwrap_or_else(|| ExprVal::Str(String::new()))
                } else {
                    ExprVal::Str(String::new())
                }
            }
            _ => ExprVal::Str(String::new()),
        }
    }
}

/// Evaluate a safe `expr` formula to a typed value. Supports arithmetic (`+ - * /`, with
/// `round(x,n)`/`abs(x)`/`min(a,b)`/`max(a,b)`), comparison (`== != < <= > >=`), boolean
/// (`&& || !`, `true`/`false`), string functions (`len/lower/upper/trim/replace/repeat/reverse/
/// contains/concat`), list functions (`sum/any/all/has/join/split/first/last`), single/double-quoted
/// string literals, parentheses, named variables, and dotted variable access (`it.author.name`).
/// `+` adds when both sides are numeric and concatenates otherwise. No side effects.
pub fn eval_expr_value(formula: &str, vars: &BTreeMap<String, ExprVal>) -> Result<ExprVal> {
    let mut toks = tokenize_expr(formula);
    match expr_or(&mut toks, vars) {
        Some(v) if toks.is_empty() => Ok(v),
        _ => Err(FlowError::Runtime(format!(
            "invalid `expr` formula: {formula}"
        ))),
    }
}

/// Statically validate an `expr` `formula` against its declared `var_keys` — the analyzer-side twin
/// of [`eval_expr_value`], reusing the SAME tokenizer/parser so the analyzer accepts exactly what
/// the runtime will (L-27). Returns one message per problem: a variable-position identifier absent
/// from `vars` (the runtime would fail to resolve it), a call to an unknown built-in function, or a
/// structurally malformed formula the parser rejects. `vars` is an explicit, unambiguous scope, so
/// this has zero false positives — `true`/`false` literals and the built-in function names are not
/// treated as variables. Dotted identifiers (L-46) have only their root checked against `var_keys`.
pub fn validate_expr_formula(formula: &str, var_keys: &BTreeSet<&str>) -> Vec<String> {
    let toks: Vec<Tok> = tokenize_expr(formula).into_iter().collect();
    let mut diags = Vec::new();
    // A complete dummy binding for every variable-position ident, so the structural parse below
    // fails only on a genuinely malformed formula — never merely on an unbound name (which gets its
    // own, more actionable diagnostic here).
    let mut dummy: BTreeMap<String, ExprVal> = BTreeMap::new();
    for (i, tok) in toks.iter().enumerate() {
        let Tok::Ident(name) = tok else { continue };
        // An ident immediately followed by `(` is a function call, not a variable read.
        if matches!(toks.get(i + 1), Some(Tok::Op(s)) if s == "(") {
            if !is_known_expr_fn(name) {
                diags.push(format!(
                    "`expr` formula calls unknown function `{name}(…)` — the built-ins are \
                     round/abs/min/max/len/lower/upper/trim/replace/repeat/reverse/contains/concat/\
                     sum/any/all/has/join/split/first/last"
                ));
            }
            continue;
        }
        // `true`/`false` are boolean literals, not variables.
        if name == "true" || name == "false" {
            continue;
        }
        // L-46: dotted identifiers — check only the root part against var_keys, and seed the dummy
        // under the ROOT (what `expr_atom` looks up before descending fields), not the full dotted
        // name. Seed dotted reads as nested objects so arithmetic like `it.score * scale` validates
        // with the same shape it will see at runtime.
        let mut parts = name.split('.');
        let root = parts.next().unwrap_or(name);
        let fields: Vec<&str> = parts.collect();
        seed_dummy_ident(&mut dummy, root, &fields);
        if !var_keys.contains(root) {
            diags.push(format!(
                "`expr` formula references `{root}` which is not declared in `vars` — add it to \
                 the `vars` map (e.g. `{{\"{root}\": $symbol}}`) so the runtime can resolve it"
            ));
        }
    }
    // Only run the structural check when nothing more specific already fired: a genuinely malformed
    // formula (e.g. a trailing operator or unbalanced parens) has no named cause above.
    if diags.is_empty() && eval_expr_value(formula, &dummy).is_err() {
        diags.push(format!(
            "`expr` formula is malformed and cannot be parsed: `{formula}`"
        ));
    }
    diags
}

fn seed_dummy_ident(dummy: &mut BTreeMap<String, ExprVal>, root: &str, fields: &[&str]) {
    if fields.is_empty() {
        dummy.entry(root.to_string()).or_insert(ExprVal::Num(1.0));
        return;
    }

    let entry = dummy
        .entry(root.to_string())
        .or_insert_with(|| ExprVal::Obj(serde_json::Map::new()));
    if !matches!(entry, ExprVal::Obj(_)) {
        *entry = ExprVal::Obj(serde_json::Map::new());
    }
    let ExprVal::Obj(map) = entry else { return };
    seed_dummy_field(map, fields);
}

fn seed_dummy_field(map: &mut serde_json::Map<String, serde_json::Value>, fields: &[&str]) {
    let Some((field, rest)) = fields.split_first() else {
        return;
    };
    if rest.is_empty() {
        map.entry((*field).to_string())
            .or_insert_with(|| serde_json::Value::Number(serde_json::Number::from(1)));
        return;
    }
    let entry = map
        .entry((*field).to_string())
        .or_insert_with(|| serde_json::Value::Object(serde_json::Map::new()));
    if !entry.is_object() {
        *entry = serde_json::Value::Object(serde_json::Map::new());
    }
    if let serde_json::Value::Object(child) = entry {
        seed_dummy_field(child, rest);
    }
}

/// A lexical token of an `expr` formula.
#[derive(Clone, Debug, PartialEq)]
pub enum Tok {
    Num(f64),
    Str(String),
    Ident(String),
    Op(String),
}

pub fn tokenize_expr(s: &str) -> VecDeque<Tok> {
    let mut tokens = VecDeque::new();
    let mut chars = s.chars().peekable();
    while let Some(&c) = chars.peek() {
        match c {
            ' ' | '\t' | '\n' | '\r' => {
                chars.next();
            }
            '0'..='9' | '.' => {
                let mut num = String::new();
                while let Some(&d) = chars.peek() {
                    if d.is_ascii_digit() || d == '.' {
                        num.push(d);
                        chars.next();
                    } else {
                        break;
                    }
                }
                match num.parse::<f64>() {
                    Ok(n) => tokens.push_back(Tok::Num(n)),
                    Err(_) => tokens.push_back(Tok::Op(num)), // malformed → fails to parse later
                }
            }
            'a'..='z' | 'A'..='Z' | '_' => {
                let mut ident = String::new();
                while let Some(&d) = chars.peek() {
                    if d.is_alphanumeric() || d == '_' {
                        ident.push(d);
                        chars.next();
                    } else {
                        break;
                    }
                }
                // L-46: continue consuming `.field` segments for dotted variable access
                while matches!(chars.peek(), Some(&'.')) {
                    // Peek ahead to ensure there's at least one alphanumeric/underscore after the dot
                    let mut peek_chars = chars.clone();
                    peek_chars.next(); // skip the '.'
                    if let Some(&next_c) = peek_chars.peek() {
                        if next_c.is_alphanumeric() || next_c == '_' {
                            ident.push('.');
                            chars.next(); // consume the '.'
                            while let Some(&d) = chars.peek() {
                                if d.is_alphanumeric() || d == '_' {
                                    ident.push(d);
                                    chars.next();
                                } else {
                                    break;
                                }
                            }
                        } else {
                            break;
                        }
                    } else {
                        break;
                    }
                }
                // L-53: absorb a trailing `?` optional-access marker on a field access (`x.field?`).
                // Field access inside an `expr` is always lenient (a missing key reads empty), so `?`
                // is a no-op here — but it must be accepted, not tokenized as an unknown op, so
                // `$x.field? == y` parses instead of failing as a "malformed formula".
                if ident.contains('.') && matches!(chars.peek(), Some(&'?')) {
                    chars.next();
                }
                tokens.push_back(Tok::Ident(ident));
            }
            '\'' | '"' => {
                let quote = c;
                chars.next();
                let mut lit = String::new();
                while let Some(d) = chars.next() {
                    if d == quote {
                        break;
                    }
                    if d == '\\' {
                        if let Some(e) = chars.next() {
                            lit.push(match e {
                                'n' => '\n',
                                't' => '\t',
                                other => other,
                            });
                        }
                    } else {
                        lit.push(d);
                    }
                }
                tokens.push_back(Tok::Str(lit));
            }
            '=' | '!' | '<' | '>' | '&' | '|' => {
                chars.next();
                let mut op = c.to_string();
                if let Some(&d) = chars.peek() {
                    let two = matches!(
                        (c, d),
                        ('=', '=') | ('!', '=') | ('<', '=') | ('>', '=') | ('&', '&') | ('|', '|')
                    );
                    if two {
                        op.push(d);
                        chars.next();
                    }
                }
                tokens.push_back(Tok::Op(op));
            }
            _ => {
                tokens.push_back(Tok::Op(c.to_string()));
                chars.next();
            }
        }
    }
    tokens
}

fn peek_op(t: &VecDeque<Tok>) -> Option<&str> {
    match t.front() {
        Some(Tok::Op(s)) => Some(s.as_str()),
        _ => None,
    }
}

fn expr_or(t: &mut VecDeque<Tok>, v: &BTreeMap<String, ExprVal>) -> Option<ExprVal> {
    let mut lhs = expr_and(t, v)?;
    while peek_op(t) == Some("||") {
        t.pop_front();
        let rhs = expr_and(t, v)?;
        lhs = ExprVal::Bool(lhs.truthy() || rhs.truthy());
    }
    Some(lhs)
}

fn expr_and(t: &mut VecDeque<Tok>, v: &BTreeMap<String, ExprVal>) -> Option<ExprVal> {
    let mut lhs = expr_cmp(t, v)?;
    while peek_op(t) == Some("&&") {
        t.pop_front();
        let rhs = expr_cmp(t, v)?;
        lhs = ExprVal::Bool(lhs.truthy() && rhs.truthy());
    }
    Some(lhs)
}

fn expr_cmp(t: &mut VecDeque<Tok>, v: &BTreeMap<String, ExprVal>) -> Option<ExprVal> {
    let lhs = expr_add(t, v)?;
    if let Some(op) = peek_op(t) {
        if matches!(op, "==" | "!=" | "<" | "<=" | ">" | ">=") {
            let op = op.to_string();
            t.pop_front();
            let rhs = expr_add(t, v)?;
            return Some(ExprVal::Bool(expr_compare(&lhs, &rhs, &op)?));
        }
    }
    Some(lhs)
}

/// Compare two values: numerically when both coerce to numbers, else lexicographically by text.
fn expr_compare(a: &ExprVal, b: &ExprVal, op: &str) -> Option<bool> {
    let res = match (a.as_num(), b.as_num()) {
        (Some(x), Some(y)) => match op {
            "==" => x == y,
            "!=" => x != y,
            "<" => x < y,
            "<=" => x <= y,
            ">" => x > y,
            ">=" => x >= y,
            _ => return None,
        },
        _ => {
            let (x, y) = (a.as_text(), b.as_text());
            match op {
                "==" => x == y,
                "!=" => x != y,
                "<" => x < y,
                "<=" => x <= y,
                ">" => x > y,
                ">=" => x >= y,
                _ => return None,
            }
        }
    };
    Some(res)
}

fn expr_add(t: &mut VecDeque<Tok>, v: &BTreeMap<String, ExprVal>) -> Option<ExprVal> {
    let mut lhs = expr_mul(t, v)?;
    loop {
        match peek_op(t) {
            Some("+") => {
                t.pop_front();
                let rhs = expr_mul(t, v)?;
                lhs = match (lhs.as_num(), rhs.as_num()) {
                    (Some(x), Some(y)) => ExprVal::Num(x + y),
                    _ => ExprVal::Str(format!("{}{}", lhs.as_text(), rhs.as_text())),
                };
            }
            Some("-") => {
                t.pop_front();
                let rhs = expr_mul(t, v)?;
                lhs = ExprVal::Num(lhs.as_num()? - rhs.as_num()?);
            }
            _ => break,
        }
    }
    Some(lhs)
}

fn expr_mul(t: &mut VecDeque<Tok>, v: &BTreeMap<String, ExprVal>) -> Option<ExprVal> {
    let mut lhs = expr_unary(t, v)?;
    loop {
        match peek_op(t) {
            Some("*") => {
                t.pop_front();
                let r = expr_unary(t, v)?;
                lhs = ExprVal::Num(lhs.as_num()? * r.as_num()?);
            }
            Some("/") => {
                t.pop_front();
                let r = expr_unary(t, v)?.as_num()?;
                if r == 0.0 {
                    return None;
                }
                lhs = ExprVal::Num(lhs.as_num()? / r);
            }
            _ => break,
        }
    }
    Some(lhs)
}

fn expr_unary(t: &mut VecDeque<Tok>, v: &BTreeMap<String, ExprVal>) -> Option<ExprVal> {
    match peek_op(t) {
        Some("-") => {
            t.pop_front();
            Some(ExprVal::Num(-expr_unary(t, v)?.as_num()?))
        }
        Some("!") => {
            t.pop_front();
            Some(ExprVal::Bool(!expr_unary(t, v)?.truthy()))
        }
        _ => expr_atom(t, v),
    }
}

fn expr_atom(t: &mut VecDeque<Tok>, v: &BTreeMap<String, ExprVal>) -> Option<ExprVal> {
    match t.pop_front()? {
        Tok::Num(n) => Some(ExprVal::Num(n)),
        Tok::Str(s) => Some(ExprVal::Str(s)),
        Tok::Op(op) if op == "(" => {
            let val = expr_or(t, v)?;
            match t.pop_front() {
                Some(Tok::Op(ref c)) if c == ")" => Some(val),
                _ => None,
            }
        }
        Tok::Op(_) => None,
        Tok::Ident(name) => {
            if matches!(t.front(), Some(Tok::Op(s)) if s == "(") {
                t.pop_front(); // consume "("
                let args = expr_call_args(t, v)?;
                expr_call_fn(&name, &args)
            } else {
                match name.as_str() {
                    "true" => Some(ExprVal::Bool(true)),
                    "false" => Some(ExprVal::Bool(false)),
                    _ => {
                        // L-46: dotted variable access — split on '.', look up root, descend into fields
                        if name.contains('.') {
                            let mut parts = name.split('.');
                            let root = parts.next()?;
                            let mut val = v.get(root).cloned()?;
                            for field in parts {
                                val = val.get_field(field);
                            }
                            Some(val)
                        } else {
                            v.get(&name).cloned()
                        }
                    }
                }
            }
        }
    }
}

/// Parse a comma-separated argument list up to and including the closing `)`.
fn expr_call_args(t: &mut VecDeque<Tok>, v: &BTreeMap<String, ExprVal>) -> Option<Vec<ExprVal>> {
    let mut args = Vec::new();
    if matches!(t.front(), Some(Tok::Op(s)) if s == ")") {
        t.pop_front();
        return Some(args);
    }
    loop {
        args.push(expr_or(t, v)?);
        match t.pop_front() {
            Some(Tok::Op(ref c)) if c == "," => continue,
            Some(Tok::Op(ref c)) if c == ")" => break,
            _ => return None,
        }
    }
    Some(args)
}

/// Apply a built-in `expr` function to its evaluated arguments.
pub fn expr_call_fn(name: &str, args: &[ExprVal]) -> Option<ExprVal> {
    match name {
        "round" => {
            let x = args.first()?.as_num()?;
            let n = args.get(1).and_then(|a| a.as_num()).unwrap_or(0.0) as i32;
            let f = 10f64.powi(n);
            Some(ExprVal::Num((x * f).round() / f))
        }
        "abs" => Some(ExprVal::Num(args.first()?.as_num()?.abs())),
        "min" => {
            if args.len() == 1 {
                // L-46: single-arg overload for list of numbers
                if let ExprVal::List(items) = args.first()? {
                    items
                        .iter()
                        .filter_map(|v| ExprVal::from_json(v).as_num())
                        .min_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal))
                        .map(ExprVal::Num)
                } else {
                    None
                }
            } else {
                Some(ExprVal::Num(
                    args.first()?.as_num()?.min(args.get(1)?.as_num()?),
                ))
            }
        }
        "max" => {
            if args.len() == 1 {
                // L-46: single-arg overload for list of numbers
                if let ExprVal::List(items) = args.first()? {
                    items
                        .iter()
                        .filter_map(|v| ExprVal::from_json(v).as_num())
                        .max_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal))
                        .map(ExprVal::Num)
                } else {
                    None
                }
            } else {
                Some(ExprVal::Num(
                    args.first()?.as_num()?.max(args.get(1)?.as_num()?),
                ))
            }
        }
        // L-35: a list counts its elements; L-46: an object counts its keys; anything else counts
        // characters of its text form (unchanged) — a bound array must not silently report the
        // length of its stringified JSON.
        "len" => Some(ExprVal::Num(match args.first()? {
            ExprVal::List(items) => items.len() as f64,
            ExprVal::Obj(map) => map.len() as f64,
            other => other.as_text().chars().count() as f64,
        })),
        "lower" => Some(ExprVal::Str(args.first()?.as_text().to_lowercase())),
        "upper" => Some(ExprVal::Str(args.first()?.as_text().to_uppercase())),
        "trim" => Some(ExprVal::Str(args.first()?.as_text().trim().to_string())),
        "reverse" => Some(ExprVal::Str(
            args.first()?.as_text().chars().rev().collect(),
        )),
        "contains" => Some(ExprVal::Bool(
            args.first()?.as_text().contains(&args.get(1)?.as_text()),
        )),
        "replace" => Some(ExprVal::Str(
            args.first()?
                .as_text()
                .replace(&args.get(1)?.as_text(), &args.get(2)?.as_text()),
        )),
        "repeat" => {
            let s = args.first()?.as_text();
            let n = args.get(1)?.as_num()?;
            if !(0.0..=100_000.0).contains(&n) || s.len().saturating_mul(n as usize) > 1_000_000 {
                return None;
            }
            Some(ExprVal::Str(s.repeat(n as usize)))
        }
        "concat" => Some(ExprVal::Str(args.iter().map(|a| a.as_text()).collect())),
        // L-46: list-aware builtins
        "sum" => {
            if let ExprVal::List(items) = args.first()? {
                let total: f64 = items
                    .iter()
                    .filter_map(|v| ExprVal::from_json(v).as_num())
                    .sum();
                Some(ExprVal::Num(total))
            } else {
                None
            }
        }
        "any" => {
            if let ExprVal::List(items) = args.first()? {
                let result = items.iter().any(|v| ExprVal::from_json(v).truthy());
                Some(ExprVal::Bool(result))
            } else {
                None
            }
        }
        "all" => {
            if let ExprVal::List(items) = args.first()? {
                let result = items.iter().all(|v| ExprVal::from_json(v).truthy());
                Some(ExprVal::Bool(result))
            } else {
                None
            }
        }
        "has" => {
            if let (ExprVal::List(items), Some(needle)) = (args.first()?, args.get(1)) {
                // For numbers, compare numeric values rather than JSON representation, since
                // Number::from(2) and Number::from_f64(2.0) may not be structurally equal.
                let result = if let ExprVal::Num(needle_num) = needle {
                    items.iter().any(|v| {
                        if let serde_json::Value::Number(n) = v {
                            n.as_f64()
                                .is_some_and(|v_num| (v_num - needle_num).abs() < f64::EPSILON)
                        } else {
                            false
                        }
                    })
                } else {
                    let needle_json = match needle {
                        ExprVal::Str(s) => serde_json::Value::String(s.clone()),
                        ExprVal::Bool(b) => serde_json::Value::Bool(*b),
                        ExprVal::List(v) => serde_json::Value::Array(v.clone()),
                        ExprVal::Obj(m) => serde_json::Value::Object(m.clone()),
                        _ => return None,
                    };
                    items.iter().any(|v| v == &needle_json)
                };
                Some(ExprVal::Bool(result))
            } else {
                None
            }
        }
        "join" => {
            if let (ExprVal::List(items), Some(sep)) = (args.first()?, args.get(1)) {
                let sep_str = sep.as_text();
                let joined = items
                    .iter()
                    .map(|v| match v {
                        serde_json::Value::String(s) => s.clone(),
                        other => serde_json::to_string(other).unwrap_or_default(),
                    })
                    .collect::<Vec<_>>()
                    .join(&sep_str);
                Some(ExprVal::Str(joined))
            } else {
                None
            }
        }
        "split" => {
            let s = args.first()?.as_text();
            let sep = args.get(1)?.as_text();
            let parts: Vec<serde_json::Value> = s
                .split(&sep)
                .map(|p| serde_json::Value::String(p.to_string()))
                .collect();
            Some(ExprVal::List(parts))
        }
        "first" => {
            if let ExprVal::List(items) = args.first()? {
                if let Some(first) = items.first() {
                    Some(ExprVal::from_json(first))
                } else {
                    Some(ExprVal::Str(String::new()))
                }
            } else {
                None
            }
        }
        "last" => {
            if let ExprVal::List(items) = args.first()? {
                if let Some(last) = items.last() {
                    Some(ExprVal::from_json(last))
                } else {
                    Some(ExprVal::Str(String::new()))
                }
            } else {
                None
            }
        }
        _ => None,
    }
}

/// Whether `name` is a built-in `expr` function (the set [`expr_call_fn`] dispatches). Used by
/// [`validate_expr_formula`] to tell a function call from an undeclared variable reference.
pub fn is_known_expr_fn(name: &str) -> bool {
    matches!(
        name,
        "round"
            | "abs"
            | "min"
            | "max"
            | "len"
            | "lower"
            | "upper"
            | "trim"
            | "reverse"
            | "contains"
            | "replace"
            | "repeat"
            | "concat"
            | "sum"
            | "any"
            | "all"
            | "has"
            | "join"
            | "split"
            | "first"
            | "last"
    )
}

/// Format a float cleanly: integer results drop the decimal, fractional keep up to 2 places.
pub fn format_number(n: f64) -> String {
    if n.fract() == 0.0 && n.abs() < 1e15 {
        format!("{}", n as i64)
    } else {
        // Up to 2 significant decimal places, strip trailing zeros.
        let s = format!("{:.2}", n);
        s.trim_end_matches('0').trim_end_matches('.').to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// L-53 (code-review #5): a dotted field access in a comparison validates — and the `?` opt-out
    /// composes with it. Before the fix, `validate_expr_formula` seeded the dummy under the full
    /// dotted name (not the root), so `eval_expr_value` couldn't resolve the root and wrongly flagged
    /// `x.field == y` "malformed"; and a `?` marker was tokenized as an unknown op, failing harder.
    #[test]
    fn dotted_comparison_and_optional_marker_validate() {
        let var_keys: BTreeSet<&str> = ["rec"].into_iter().collect();
        assert!(
            validate_expr_formula("rec.a == 1", &var_keys).is_empty(),
            "a dotted comparison must validate: {:?}",
            validate_expr_formula("rec.a == 1", &var_keys)
        );
        assert!(
            validate_expr_formula("rec.a * 2 > 1", &var_keys).is_empty(),
            "dotted arithmetic must validate: {:?}",
            validate_expr_formula("rec.a * 2 > 1", &var_keys)
        );
        assert!(
            validate_expr_formula("rec.missing? == \"z\"", &var_keys).is_empty(),
            "the `?` opt-out must compose with a comparison: {:?}",
            validate_expr_formula("rec.missing? == \"z\"", &var_keys)
        );
        // The `?` is a no-op in expr (field access there is already lenient): the tokenizer drops it,
        // so both forms tokenize identically.
        assert_eq!(
            tokenize_expr("rec.missing? == \"z\"").len(),
            tokenize_expr("rec.missing == \"z\"").len(),
        );
    }

    #[test]
    fn expr_dotted_access_descends_objects_and_nulls_missing() {
        let mut vars = BTreeMap::new();
        vars.insert(
            "it".to_string(),
            ExprVal::from_json(&serde_json::json!({
                "author": {
                    "name": "Alice",
                    "age": 30
                },
                "title": "Hello"
            })),
        );

        // Descend into nested object
        let result = eval_expr_value("it.author.name", &vars).unwrap();
        assert_eq!(result, ExprVal::Str("Alice".to_string()));

        let result = eval_expr_value("it.author.age", &vars).unwrap();
        assert_eq!(result, ExprVal::Num(30.0));

        // Missing field returns empty string
        let result = eval_expr_value("it.author.missing", &vars).unwrap();
        assert_eq!(result, ExprVal::Str(String::new()));

        // Partial path works
        let result = eval_expr_value("it.title", &vars).unwrap();
        assert_eq!(result, ExprVal::Str("Hello".to_string()));
    }

    #[test]
    fn expr_list_builtins_semantics() {
        let mut vars = BTreeMap::new();
        vars.insert(
            "nums".to_string(),
            ExprVal::List(vec![
                serde_json::Value::Number(serde_json::Number::from(1)),
                serde_json::Value::Number(serde_json::Number::from(2)),
                serde_json::Value::Number(serde_json::Number::from(3)),
            ]),
        );
        vars.insert(
            "mixed".to_string(),
            ExprVal::List(vec![
                serde_json::Value::Bool(true),
                serde_json::Value::Bool(false),
                serde_json::Value::String("yes".to_string()),
            ]),
        );
        vars.insert("empty".to_string(), ExprVal::List(vec![]));

        // sum
        let result = eval_expr_value("sum(nums)", &vars).unwrap();
        assert_eq!(result, ExprVal::Num(6.0));

        // any
        let result = eval_expr_value("any(mixed)", &vars).unwrap();
        assert_eq!(result, ExprVal::Bool(true));

        let result = eval_expr_value("any(empty)", &vars).unwrap();
        assert_eq!(result, ExprVal::Bool(false));

        // all
        let result = eval_expr_value("all(nums)", &vars).unwrap();
        assert_eq!(result, ExprVal::Bool(true));

        let result = eval_expr_value("all(mixed)", &vars).unwrap();
        assert_eq!(result, ExprVal::Bool(false)); // false is in the list

        let result = eval_expr_value("all(empty)", &vars).unwrap();
        assert_eq!(result, ExprVal::Bool(true)); // empty = all true

        // has
        let result = eval_expr_value("has(nums, 2)", &vars).unwrap();
        assert_eq!(result, ExprVal::Bool(true));

        let result = eval_expr_value("has(nums, 5)", &vars).unwrap();
        assert_eq!(result, ExprVal::Bool(false));

        // join
        vars.insert(
            "words".to_string(),
            ExprVal::List(vec![
                serde_json::Value::String("hello".to_string()),
                serde_json::Value::String("world".to_string()),
            ]),
        );
        let result = eval_expr_value("join(words, ' ')", &vars).unwrap();
        assert_eq!(result, ExprVal::Str("hello world".to_string()));

        // first
        let result = eval_expr_value("first(nums)", &vars).unwrap();
        assert_eq!(result, ExprVal::Num(1.0));

        let result = eval_expr_value("first(empty)", &vars).unwrap();
        assert_eq!(result, ExprVal::Str(String::new()));

        // last
        let result = eval_expr_value("last(nums)", &vars).unwrap();
        assert_eq!(result, ExprVal::Num(3.0));

        let result = eval_expr_value("last(empty)", &vars).unwrap();
        assert_eq!(result, ExprVal::Str(String::new()));

        // min/max with list
        let result = eval_expr_value("min(nums)", &vars).unwrap();
        assert_eq!(result, ExprVal::Num(1.0));

        let result = eval_expr_value("max(nums)", &vars).unwrap();
        assert_eq!(result, ExprVal::Num(3.0));
    }

    #[test]
    fn expr_split_returns_list_val() {
        let mut vars = BTreeMap::new();
        vars.insert("s".to_string(), ExprVal::Str("a,b,c".to_string()));

        let result = eval_expr_value("split(s, ',')", &vars).unwrap();
        match result {
            ExprVal::List(items) => {
                assert_eq!(items.len(), 3);
                assert_eq!(items[0], serde_json::Value::String("a".to_string()));
                assert_eq!(items[1], serde_json::Value::String("b".to_string()));
                assert_eq!(items[2], serde_json::Value::String("c".to_string()));
            }
            _ => panic!("Expected List"),
        }

        // Can use len() on the result
        let result = eval_expr_value("len(split(s, ','))", &vars).unwrap();
        assert_eq!(result, ExprVal::Num(3.0));
    }

    #[test]
    fn expr_val_obj_truthiness_and_render() {
        let mut obj_map = serde_json::Map::new();
        obj_map.insert(
            "key".to_string(),
            serde_json::Value::String("value".to_string()),
        );
        let obj = ExprVal::Obj(obj_map.clone());

        // Non-empty object is truthy
        assert!(obj.truthy());

        // Empty object is falsy
        let empty_obj = ExprVal::Obj(serde_json::Map::new());
        assert!(!empty_obj.truthy());

        // as_text renders compact JSON
        let text = obj.as_text();
        assert_eq!(text, r#"{"key":"value"}"#);

        // len counts keys
        let mut vars = BTreeMap::new();
        vars.insert("obj".to_string(), obj);
        let result = eval_expr_value("len(obj)", &vars).unwrap();
        assert_eq!(result, ExprVal::Num(1.0));

        vars.insert("empty".to_string(), empty_obj);
        let result = eval_expr_value("len(empty)", &vars).unwrap();
        assert_eq!(result, ExprVal::Num(0.0));
    }
}
