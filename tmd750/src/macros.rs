//! Repeated standard interfaces for checked scalar and text domains.

macro_rules! integer_formats {
    ($wrapper:ty, $view:ident) => {
        impl std::fmt::Binary for $wrapper {
            fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                std::fmt::Binary::fmt(&self.$view(), formatter)
            }
        }
        impl std::fmt::Octal for $wrapper {
            fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                std::fmt::Octal::fmt(&self.$view(), formatter)
            }
        }
        impl std::fmt::LowerHex for $wrapper {
            fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                std::fmt::LowerHex::fmt(&self.$view(), formatter)
            }
        }
        impl std::fmt::UpperHex for $wrapper {
            fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                std::fmt::UpperHex::fmt(&self.$view(), formatter)
            }
        }
    };
}

macro_rules! checked_integer_traits {
    ($wrapper:ty, $raw:ty, $view:ident) => {
        checked_integer_traits!($wrapper, $raw, $view, crate::ValidationError);
    };
    ($wrapper:ty, $raw:ty, $view:ident, $error:ty) => {
        impl TryFrom<$raw> for $wrapper {
            type Error = $error;
            fn try_from(value: $raw) -> Result<Self, Self::Error> {
                Self::new(value)
            }
        }
        impl From<$wrapper> for $raw {
            fn from(value: $wrapper) -> Self {
                value.$view()
            }
        }
        integer_formats!($wrapper, $view);
    };
}

macro_rules! unnamed_wire_value {
    ($(#[$doc:meta])* $name:ident, [$named:pat]) => {
        $(#[$doc])*
        #[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
        pub struct $name(u8);

        impl $name {
            /// Retain an unnamed byte, rejecting every byte with a named variant.
            ///
            /// # Errors
            /// Returns [`crate::ValidationError::NamedWireValue`] for a named byte.
            pub const fn new(value: u8) -> Result<Self, crate::ValidationError> {
                match value {
                    $named => Err(crate::ValidationError::NamedWireValue { value }),
                    _ => Ok(Self(value)),
                }
            }

            /// The unchanged received byte.
            #[must_use]
            pub const fn get(self) -> u8 { self.0 }
        }

        impl std::fmt::Display for $name {
            fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                std::fmt::Display::fmt(&self.0, formatter)
            }
        }

        checked_integer_traits!($name, u8, get);
    };
}

macro_rules! packed_byte {
    ($(#[$doc:meta])* $name:ident, $maximum:literal) => {
        $(#[$doc])*
        #[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Default)]
        pub struct $name(u8);

        impl $name {
            /// All bits clear.
            pub const ZERO: Self = Self(0);

            /// Retain a value that fits the field's complete stored width.
            ///
            /// # Errors
            /// Returns [`crate::ValidationError::PackedValueOutOfRange`] if bits
            /// outside that width are set.
            pub const fn new(value: u8) -> Result<Self, crate::ValidationError> {
                if value <= $maximum { Ok(Self(value)) } else {
                    Err(crate::ValidationError::PackedValueOutOfRange {
                        value, maximum: $maximum,
                    })
                }
            }

            /// The unchanged field value.
            #[must_use]
            pub const fn get(self) -> u8 { self.0 }
        }

        impl std::fmt::Display for $name {
            fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                std::fmt::Display::fmt(&self.0, formatter)
            }
        }

        checked_integer_traits!($name, u8, get);
    };
}

macro_rules! string_traits {
    ($wrapper:ty, $error:ty) => {
        impl AsRef<str> for $wrapper {
            fn as_ref(&self) -> &str {
                self.as_str()
            }
        }
        impl std::str::FromStr for $wrapper {
            type Err = $error;
            fn from_str(value: &str) -> Result<Self, Self::Err> {
                Self::new(value)
            }
        }
        impl TryFrom<&str> for $wrapper {
            type Error = $error;
            fn try_from(value: &str) -> Result<Self, Self::Error> {
                Self::new(value)
            }
        }
        impl TryFrom<String> for $wrapper {
            type Error = $error;
            fn try_from(value: String) -> Result<Self, Self::Error> {
                Self::validate(&value)?;
                Ok(Self(value))
            }
        }
        impl From<$wrapper> for String {
            fn from(value: $wrapper) -> Self {
                value.0
            }
        }
    };
}
