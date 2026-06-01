//! Internal declarative macros that generate the typed vocabulary.
//!
//! These are the single source of truth for string-backed enums and string
//! newtypes: every code-graph kind, identifier, and reference category is
//! declared once here and gains a stable storage spelling, an inverse parse,
//! and matching serde behavior.

/// Counts the token trees passed to it as a `const usize` expression.
#[macro_export]
macro_rules! count {
    () => (0usize);
    ($_head:tt $($tail:tt)*) => (1usize + $crate::count!($($tail)*));
}

/// Declares a closed enum backed by stable string spellings.
///
/// Generates `as_str`, an `ALL` array over the primary variants, `Display`,
/// `FromStr`/`TryFrom<&str>` (the inverse the read path needs), and serde
/// `Serialize`/`Deserialize` as the plain spelling. An optional `extra { .. }`
/// block adds variants that round-trip but are excluded from `ALL` (e.g. a
/// diagnostic pseudo-kind). An optional `default = Variant;` adds `Default`.
macro_rules! string_enum {
    (
        $(#[$meta:meta])*
        $vis:vis enum $name:ident {
            $( $(#[$vmeta:meta])* $variant:ident => $repr:literal ),+ $(,)?
        }
        $( extra { $( $(#[$xmeta:meta])* $xvariant:ident => $xrepr:literal ),+ $(,)? } )?
        $( default = $default:ident; )?
    ) => {
        $(#[$meta])*
        #[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
        $vis enum $name {
            $( $(#[$vmeta])* $variant, )+
            $( $( $(#[$xmeta])* $xvariant, )+ )?
        }

        impl $name {
            /// All primary variants, in declaration order.
            $vis const ALL: [Self; $crate::count!($($variant)+)] = [ $( Self::$variant, )+ ];

            /// Returns the stable storage spelling.
            #[must_use]
            $vis const fn as_str(self) -> &'static str {
                match self {
                    $( Self::$variant => $repr, )+
                    $( $( Self::$xvariant => $xrepr, )+ )?
                }
            }
        }

        impl ::core::fmt::Display for $name {
            fn fmt(&self, formatter: &mut ::core::fmt::Formatter<'_>) -> ::core::fmt::Result {
                formatter.write_str(self.as_str())
            }
        }

        impl ::core::str::FromStr for $name {
            type Err = $crate::UnknownVariant;

            fn from_str(value: &str) -> ::core::result::Result<Self, Self::Err> {
                match value {
                    $( $repr => ::core::result::Result::Ok(Self::$variant), )+
                    $( $( $xrepr => ::core::result::Result::Ok(Self::$xvariant), )+ )?
                    other => ::core::result::Result::Err($crate::UnknownVariant {
                        kind: stringify!($name),
                        value: other.to_owned(),
                    }),
                }
            }
        }

        impl ::core::convert::TryFrom<&str> for $name {
            type Error = $crate::UnknownVariant;

            fn try_from(value: &str) -> ::core::result::Result<Self, Self::Error> {
                value.parse()
            }
        }

        impl ::serde::Serialize for $name {
            fn serialize<S>(&self, serializer: S) -> ::core::result::Result<S::Ok, S::Error>
            where
                S: ::serde::Serializer,
            {
                serializer.serialize_str(self.as_str())
            }
        }

        impl<'de> ::serde::Deserialize<'de> for $name {
            fn deserialize<D>(deserializer: D) -> ::core::result::Result<Self, D::Error>
            where
                D: ::serde::Deserializer<'de>,
            {
                let raw = <::std::string::String as ::serde::Deserialize>::deserialize(deserializer)?;
                raw.parse().map_err(::serde::de::Error::custom)
            }
        }

        $(
            impl ::core::default::Default for $name {
                fn default() -> Self {
                    Self::$default
                }
            }
        )?
    };
}

/// Declares a transparent `String` newtype with the conversions oxcode relies on.
macro_rules! string_newtype {
    ($name:ident, $doc:literal) => {
        #[doc = $doc]
        #[derive(
            Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash,
            serde::Serialize, serde::Deserialize,
        )]
        #[serde(transparent)]
        pub struct $name(String);

        impl $name {
            /// Creates a new value.
            #[must_use]
            pub fn new(value: impl Into<String>) -> Self {
                Self(value.into())
            }

            /// Returns the underlying string.
            #[must_use]
            pub fn as_str(&self) -> &str {
                &self.0
            }

            /// Consumes the newtype and returns the owned string.
            #[must_use]
            pub fn into_inner(self) -> String {
                self.0
            }
        }

        impl ::core::convert::From<&str> for $name {
            fn from(value: &str) -> Self {
                Self::new(value)
            }
        }

        impl ::core::convert::From<String> for $name {
            fn from(value: String) -> Self {
                Self::new(value)
            }
        }

        impl ::core::convert::AsRef<str> for $name {
            fn as_ref(&self) -> &str {
                &self.0
            }
        }

        impl ::core::borrow::Borrow<str> for $name {
            fn borrow(&self) -> &str {
                &self.0
            }
        }

        impl ::core::cmp::PartialEq<str> for $name {
            fn eq(&self, other: &str) -> bool {
                self.0 == other
            }
        }

        impl ::core::cmp::PartialEq<&str> for $name {
            fn eq(&self, other: &&str) -> bool {
                self.0 == *other
            }
        }

        impl ::core::fmt::Display for $name {
            fn fmt(&self, formatter: &mut ::core::fmt::Formatter<'_>) -> ::core::fmt::Result {
                formatter.write_str(self.as_str())
            }
        }
    };
}
