//! Standard integer formatting for checked scalar wrappers.

macro_rules! integer_radix_formats {
    ($type:ty, $accessor:ident) => {
        impl core::fmt::Binary for $type {
            fn fmt(&self, formatter: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
                core::fmt::Binary::fmt(&self.$accessor(), formatter)
            }
        }
        impl core::fmt::Octal for $type {
            fn fmt(&self, formatter: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
                core::fmt::Octal::fmt(&self.$accessor(), formatter)
            }
        }
        impl core::fmt::LowerHex for $type {
            fn fmt(&self, formatter: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
                core::fmt::LowerHex::fmt(&self.$accessor(), formatter)
            }
        }
        impl core::fmt::UpperHex for $type {
            fn fmt(&self, formatter: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
                core::fmt::UpperHex::fmt(&self.$accessor(), formatter)
            }
        }
    };
}
