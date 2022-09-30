use eyre::*;
use std::cmp::Ordering;
use std::collections::HashMap;

use super::generator::{Builtin, Function, FunctionClass};
use super::parser::{AstNode, Token};

const MODULE_SEPARATOR: &str = "__";
const ARRAY_SEPARATOR: &str = "_";

lazy_static::lazy_static! {
    pub static ref BUILTINS: HashMap<&'static str, Function> = maplit::hashmap!{
        "nth" => Function {
            name: "nth".into(),
            class: FunctionClass::Builtin(Builtin::Nth),
        },

        "for" => Function {
            name: "for".into(),
            class: FunctionClass::SpecialForm(Form::For),
        },


        // monadic
        "inv" => Function{
            name: "inv".into(),
            class: FunctionClass::Builtin(Builtin::Inv)
        },
        "neg" => Function{
            name: "neg".into(),
            class: FunctionClass::Builtin(Builtin::Neg)
        },

        // Dyadic
        "shift" => Function{
            name: "shift".into(),
            class: FunctionClass::Builtin(Builtin::Shift),
        },


        // polyadic
        "+" => Function {
            name: "+".into(),
            class: FunctionClass::Builtin(Builtin::Add)
        },
        "*" => Function {
            name: "*".into(),
            class: FunctionClass::Builtin(Builtin::Mul)
        },
        "-" => Function {
            name: "-".into(),
            class: FunctionClass::Builtin(Builtin::Sub)
        },

        "begin" => Function{name: "begin".into(), class: FunctionClass::Builtin(Builtin::Begin)},

        "if-zero" => Function {
            name: "if-zero".into(),
            class: FunctionClass::Builtin(Builtin::IfZero)
        },
        "if-not-zero" => Function {
            name: "if-not-zero".into(),
            class: FunctionClass::Builtin(Builtin::IfNotZero)
        },

        "make-decomposition" => Function {
            name: "make-decomposition".into(),
            class: FunctionClass::Builtin(Builtin::ByteDecomposition)
        }
    };
}

// Form have a special treatment and do not evaluate all their arguments
#[derive(Debug, PartialEq, Clone, Copy)]
pub enum Form {
    For,
}

pub enum Arity {
    AtLeast(usize),
    // AtMost(usize),
    // Even,
    // Odd,
    Monadic,
    Dyadic,
    Exactly(usize),
    Between(usize, usize),
}
impl Arity {
    fn make_error(&self, l: usize) -> String {
        fn arg_count(x: usize) -> String {
            format!("{} argument{}", x, if x > 1 { "s" } else { "" })
        }
        match self {
            Arity::AtLeast(x) => format!("expected at least {}, but received {}", arg_count(*x), l),
            // Arity::AtMost(x) => format!("expected at most {}, but received {}", arg_count(*x), l),
            // Arity::Even => format!("expected an even numer of arguments, but received {}", l),
            // Arity::Odd => format!("expected an odd numer of arguments, but received {}", l),
            Arity::Monadic => format!("expected {}, but received {}", arg_count(1), l),
            Arity::Dyadic => format!("expected {}, but received {}", arg_count(2), l),
            Arity::Exactly(x) => format!("expected {}, but received {}", arg_count(*x), l),
            Arity::Between(x, y) => format!(
                "expected between {} and {}, but received {}",
                arg_count(*x),
                arg_count(*y),
                l
            ),
        }
    }

    fn validate(&self, l: usize) -> Result<()> {
        match self {
            Arity::AtLeast(x) => l >= *x,
            // Arity::AtMost(x) => l <= *x,
            // Arity::Even => l % 2 == 0,
            // Arity::Odd => l % 2 == 1,
            Arity::Monadic => l == 1,
            Arity::Dyadic => l == 2,
            Arity::Exactly(x) => l == *x,
            Arity::Between(x, y) => l >= *x && l <= *y,
        }
        .then(|| ())
        .ok_or_else(|| eyre!(self.make_error(l)))
    }
}
pub trait FuncVerifier<T> {
    fn arity(&self) -> Arity;

    fn validate_types(&self, args: &[T]) -> Result<()>;

    fn validate_arity(&self, args: &[T]) -> Result<()> {
        self.arity().validate(args.len())
    }

    fn validate_args(&self, args: Vec<T>) -> Result<Vec<T>> {
        self.validate_arity(&args)
            .and_then(|_| self.validate_types(&args))
            .and(Ok(args))
    }
}

impl FuncVerifier<AstNode> for Form {
    fn arity(&self) -> Arity {
        match self {
            Form::For => Arity::Exactly(3),
        }
    }
    fn validate_types(&self, args: &[AstNode]) -> Result<()> {
        match self {
            Form::For => {
                if matches!(args[0].class, Token::Symbol(_))
                    && matches!(args[1].class, Token::Range(_))
                    && matches!(args[2].class, Token::List { .. })
                {
                    Ok(())
                } else {
                    Err(eyre!(
                        "`{:?}` expects [SYMBOL VALUE] but received {:?}",
                        self,
                        args
                    ))
                }
            }
        }
    }
}

/// The type of a column in the IR
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Type {
    Numeric,
    Boolean,
    Void,
}

impl std::cmp::Ord for Type {
    fn cmp(&self, other: &Self) -> Ordering {
        match (self, other) {
            (Type::Numeric, Type::Boolean) => Ordering::Greater,
            (Type::Boolean, Type::Numeric) => Ordering::Less,
            _ => Ordering::Equal,
        }
    }
}

impl std::cmp::PartialOrd for Type {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        match (self, other) {
            (Type::Numeric, Type::Boolean) => Some(Ordering::Greater),
            (Type::Boolean, Type::Numeric) => Some(Ordering::Less),
            _ => Some(Ordering::Equal),
        }
    }
}

#[derive(Clone, PartialEq, Eq, Hash)]
pub struct Handle {
    pub module: String,
    pub name: String,
}
impl Handle {
    pub fn new<S1: AsRef<str>, S2: AsRef<str>>(module: S1, name: S2) -> Self {
        Handle {
            module: module.as_ref().to_owned(),
            name: name.as_ref().to_owned(),
        }
    }

    pub fn ith(&self, i: usize) -> Handle {
        Handle {
            module: self.module.clone(),
            name: format!("{}{}{}", self.name, ARRAY_SEPARATOR, i),
        }
    }

    fn purify(s: &str) -> String {
        s.replace('(', "_")
            .replace(')', "_")
            .replace('{', "_")
            .replace('}', "_")
            .replace('[', "_")
            .replace(']', "_")
            .replace('/', "_")
            .replace(':', "_")
            .replace('%', "_")
            .replace('.', "_")
    }

    pub fn mangle(&self) -> String {
        format!(
            "{}{}{}",
            Self::purify(&self.module),
            if self.module.is_empty() {
                ""
            } else {
                MODULE_SEPARATOR
            },
            Self::purify(&self.name)
        )
    }

    pub fn mangle_no_module(&self) -> String {
        Self::purify(&self.name)
    }
}
impl std::fmt::Debug for Handle {
    fn fmt(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
        write!(f, "{}::{}", self.module, self.name)
    }
}
impl std::fmt::Display for Handle {
    fn fmt(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
        write!(f, "{}.{}", self.module, self.name)
    }
}
