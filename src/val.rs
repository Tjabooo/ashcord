//! One value type for both gateway encodings (JSON and Erlang ETF), so plugins are written once.
//!
//! ETF keeps details JSON doesn't have (atoms vs binaries, 64-bit integers instead of string snowflakes).
//! Values remember those details, so a payload decoded from ETF encodes back to the same shape.

#[derive(Clone, Debug, PartialEq)]
pub enum Val {
    Null,
    Bool(bool),
    Int(i64),
    Big(i128),
    Float(f64),
    Str(String),
    Atom(String),
    Bin(Vec<u8>),
    Charlist(Vec<u8>),
    List(Vec<Val>),
    Tuple(Vec<Val>),
    Map(Vec<(Val, Val)>),
}

impl Val {
    pub fn map() -> Val {
        Val::Map(Vec::new())
    }

    pub fn as_str(&self) -> Option<&str> {
        match self {
            Val::Str(s) | Val::Atom(s) => Some(s),
            _ => None,
        }
    }

    pub fn as_i64(&self) -> Option<i64> {
        match self {
            Val::Int(n) => Some(*n),
            Val::Big(n) => i64::try_from(*n).ok(),
            Val::Float(f) if f.fract() == 0.0 => Some(*f as i64),
            _ => None,
        }
    }

    pub fn as_bool(&self) -> Option<bool> {
        match self {
            Val::Bool(b) => Some(*b),
            _ => None,
        }
    }

    /// Discord IDs: strings in JSON, integers in ETF.
    pub fn snowflake(&self) -> Option<u64> {
        match self {
            Val::Str(s) => s.parse().ok(),
            Val::Int(n) => u64::try_from(*n).ok(),
            Val::Big(n) => u64::try_from(*n).ok(),
            _ => None,
        }
    }

    pub fn as_list(&self) -> &[Val] {
        match self {
            Val::List(v) | Val::Tuple(v) => v,
            _ => &[],
        }
    }

    pub fn as_list_mut(&mut self) -> Option<&mut Vec<Val>> {
        match self {
            Val::List(v) => Some(v),
            _ => None,
        }
    }

    fn key_is(k: &Val, key: &str) -> bool {
        match k {
            Val::Str(s) | Val::Atom(s) => s == key,
            Val::Bin(b) => b == key.as_bytes(),
            _ => false,
        }
    }

    pub fn get(&self, key: &str) -> Option<&Val> {
        match self {
            Val::Map(m) => m.iter().find(|(k, _)| Self::key_is(k, key)).map(|(_, v)| v),
            _ => None,
        }
    }

    pub fn get_mut(&mut self, key: &str) -> Option<&mut Val> {
        match self {
            Val::Map(m) => m.iter_mut().find(|(k, _)| Self::key_is(k, key)).map(|(_, v)| v),
            _ => None,
        }
    }

    /// Path lookup: `v.at(&["d", "user", "id"])`.
    pub fn at(&self, path: &[&str]) -> Option<&Val> {
        path.iter().try_fold(self, |v, k| v.get(k))
    }

    pub fn str_at(&self, path: &[&str]) -> Option<&str> {
        self.at(path).and_then(Val::as_str)
    }

    pub fn id_at(&self, path: &[&str]) -> Option<u64> {
        self.at(path).and_then(Val::snowflake)
    }

    /// Set a key in a map, keeping the key style (atom or string) the map already uses.
    pub fn set(&mut self, key: &str, val: Val) {
        if let Val::Map(m) = self {
            if let Some(slot) = m.iter_mut().find(|(k, _)| Self::key_is(k, key)) {
                slot.1 = val;
                return;
            }
            let k = match m.first() {
                Some((Val::Atom(_), _)) => Val::Atom(key.into()),
                _ => Val::Str(key.into()),
            };
            m.push((k, val));
        }
    }

    /// A string of the same kind as `self` (an atom stays an atom), for replacing e.g. the event name.
    pub fn str_like(&self, s: &str) -> Val {
        match self {
            Val::Atom(_) => Val::Atom(s.into()),
            _ => Val::Str(s.into()),
        }
    }

    pub fn from_json(v: serde_json::Value) -> Val {
        use serde_json::Value as J;
        match v {
            J::Null => Val::Null,
            J::Bool(b) => Val::Bool(b),
            J::Number(n) => {
                if let Some(i) = n.as_i64() {
                    Val::Int(i)
                } else if let Some(u) = n.as_u64() {
                    Val::Big(u as i128)
                } else {
                    Val::Float(n.as_f64().unwrap_or(0.0))
                }
            }
            J::String(s) => Val::Str(s),
            J::Array(a) => Val::List(a.into_iter().map(Val::from_json).collect()),
            J::Object(o) => Val::Map(o.into_iter().map(|(k, v)| (Val::Str(k), Val::from_json(v))).collect()),
        }
    }

    pub fn to_json(&self) -> serde_json::Value {
        use serde_json::Value as J;
        match self {
            Val::Null => J::Null,
            Val::Bool(b) => J::Bool(*b),
            Val::Int(n) => J::from(*n),
            Val::Big(n) => u64::try_from(*n).map(J::from).unwrap_or_else(|_| J::String(n.to_string())),
            Val::Float(f) => serde_json::Number::from_f64(*f).map(J::Number).unwrap_or(J::Null),
            Val::Str(s) | Val::Atom(s) => J::String(s.clone()),
            Val::Bin(b) | Val::Charlist(b) => J::String(String::from_utf8_lossy(b).into_owned()),
            Val::List(v) | Val::Tuple(v) => J::Array(v.iter().map(Val::to_json).collect()),
            Val::Map(m) => J::Object(
                m.iter()
                    .map(|(k, v)| {
                        let key = match k {
                            Val::Str(s) | Val::Atom(s) => s.clone(),
                            other => other.to_json().to_string(),
                        };
                        (key, v.to_json())
                    })
                    .collect(),
            ),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn json_roundtrip_keeps_order_and_ids() {
        let src = r#"{"op":0,"t":"MESSAGE_CREATE","d":{"id":"1170000000000000000","n":1.5}}"#;
        let v = Val::from_json(serde_json::from_str(src).unwrap());
        assert_eq!(v.id_at(&["d", "id"]), Some(1170000000000000000));
        assert_eq!(serde_json::to_string(&v.to_json()).unwrap(), src);
    }

    #[test]
    fn set_keeps_key_style() {
        let mut m = Val::Map(vec![(Val::Atom("a".into()), Val::Int(1))]);
        m.set("b", Val::Int(2));
        assert!(matches!(m, Val::Map(ref v) if v[1].0 == Val::Atom("b".into())));
        m.set("a", Val::Int(3));
        assert_eq!(m.get("a"), Some(&Val::Int(3)));
    }
}
