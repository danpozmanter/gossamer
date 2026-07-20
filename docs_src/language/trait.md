# `lang::trait`

Behaviour interface declaration.
<!-- hand-maintained from here: preserved by `gos doc --emit-stdlib` -->

A trait declares method signatures; a type provides them with `impl Trait
for Type`. A generic function bounds a parameter by a trait and calls its
methods (`fn report<T: Shape>(s: &T)`) - see [generics](generics.md).

```gossamer
trait Area { fn area(&self) -> f64; }

impl Area for Shape {
    fn area(&self) -> f64 {
        match self {
            Shape::Circle(r) => 3.14159 * r * r,
            Shape::Rect { w, h } => w * h,
        }
    }
}
```

## Operator overloading

Implementing the matching trait makes an operator dispatch to its method
on a user struct, enum, or generic struct - on every tier (bytecode VM,
JIT, and LLVM AOT alike). The result type is the method's return type, so
a dot product (`Mul -> f64`) types correctly. Compound assignment
(`a += b`) routes through the same binary method.

| Operator | Trait | Method |
|---|---|---|
| `a + b` / `a - b` / `a * b` / `a / b` | `Add` / `Sub` / `Mul` / `Div` | `add` / `sub` / `mul` / `div` |
| `a % b` | `Rem` | `rem` |
| `-a` (unary) | `Neg` | `neg` |
| `a[i]` | `Index` | `index` |
| `a | b` / `a & b` / `a ^ b` | `BitOr` / `BitAnd` / `BitXor` | `bitor` / `bitand` / `bitxor` |
| `a << b` / `a >> b` | `Shl` / `Shr` | `shl` / `shr` |

```gossamer
struct V2 { x: f64, y: f64 }

impl Add for V2 { fn add(self, o: V2) -> V2 { V2 { x: self.x + o.x, y: self.y + o.y } } }
impl Mul for V2 { fn mul(self, o: V2) -> f64 { self.x * o.x + self.y * o.y } }
```

Applying an arithmetic operator to an ADT with no matching impl is a
compile error (`GT0003`). These are real impls, not derives - the operator
traits are **not** `#[derive]`-able.

## Conversions: `From` / `TryFrom`

A `from` impl powers both `B::from(x)` and `x.into()`; a `try_from` impl
powers `B::try_from(x)` and `x.try_into() -> Result<B, E>`. The `into` /
`try_into` target is inferred from the use site (`let B`, a `B` parameter
or return):

```gossamer
struct Celsius { t: i64 }
struct Fahrenheit { t: i64 }

impl Fahrenheit {
    fn from(c: Celsius) -> Fahrenheit { Fahrenheit { t: c.t * 9 / 5 + 32 } }
}

let f: Fahrenheit = Celsius { t: 100 }.into()   // 212, via Fahrenheit::from
```

`?` also auto-converts a propagated `Err` through `errors::Error::from`,
so a `Result<_, String>` flows into a `Result<_, errors::Error>` function
with no explicit `map_err`.
