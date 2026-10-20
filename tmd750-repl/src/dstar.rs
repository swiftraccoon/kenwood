//! Checked startup requests and captured CAT/MMDVM diagnostics.
//!
//! The application refuses the unsupported Kenwood Terminal configuration
//! before discovery, capture creation or radio I/O. The diagnostic command
//! retains its separate guarded Terminal lifecycle.

use std::fmt;

use dstar_gateway_core::{Module, RoutingCallsign, TypeError};
use mmdvm::{ConfigurationError, ConfigurationProfile};

pub(crate) mod probe;

/// Fully validated arguments for the unavailable `dstar start` operation.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct StartRequest {
    callsign: RoutingCallsign,
    reflector: Option<LinkArg>,
}

/// Invalid startup syntax or checked identity/module value.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub(super) enum StartParseError {
    #[error("callsign required; expected dstar start <callsign> [reflector]")]
    MissingCallsign,
    #[error("too many arguments; expected dstar start <callsign> [reflector]")]
    TooManyArguments,
    #[error("invalid reflector {0:?}; expected a name and module such as REF030C")]
    Reflector(String),
    #[error("local module prefix must be one letter, not {0:?}")]
    LocalPrefix(String),
    #[error("invalid D-STAR routing identity or module: {0}")]
    Routing(#[from] TypeError),
    #[error("invalid modem identity: {0}")]
    Identity(#[from] mmdvm::dstar::DstarConfigError),
}

impl StartRequest {
    /// Validate startup syntax before any endpoint selection or I/O.
    pub(super) fn parse(arguments: &[&str]) -> Result<Self, StartParseError> {
        let raw_callsign = arguments.first().ok_or(StartParseError::MissingCallsign)?;
        if arguments.len() > 2 {
            return Err(StartParseError::TooManyArguments);
        }
        let station = raw_callsign.to_ascii_uppercase();
        let _identity = mmdvm::dstar::DstarModemConfig::new(&station)?;
        let callsign = RoutingCallsign::new(&station)?;
        let reflector = arguments
            .get(1)
            .map(|value| LinkArg::parse(&value.to_ascii_uppercase()))
            .transpose()?;
        Ok(Self {
            callsign,
            reflector,
        })
    }

    /// Return the application's unsupported-profile refusal without side effects.
    pub(super) const fn refuse(self) -> StartRefusal {
        StartRefusal {
            request: self,
            source: ConfigurationError::UnsupportedProfile {
                profile: ConfigurationProfile::KenwoodTerminal,
            },
        }
    }
}

/// Requested reflector and local-module selection.
#[derive(Clone, Debug, PartialEq, Eq)]
struct LinkArg {
    reflector_name: RoutingCallsign,
    reflector_module: Module,
    local_module: Module,
}

impl LinkArg {
    fn parse(value: &str) -> Result<Self, StartParseError> {
        let (local_prefix, reflector) = value
            .split_once(':')
            .map_or((None, value), |(left, right)| (Some(left), right));
        if reflector.len() < 4 {
            return Err(StartParseError::Reflector(reflector.to_owned()));
        }
        let module_char = reflector
            .chars()
            .last()
            .ok_or_else(|| StartParseError::Reflector(reflector.to_owned()))?;
        let name_end = reflector.len().saturating_sub(module_char.len_utf8());
        let name = reflector
            .get(..name_end)
            .ok_or_else(|| StartParseError::Reflector(reflector.to_owned()))?;
        let reflector_module = Module::try_from_char(module_char)?;
        let reflector_name = RoutingCallsign::new(name)?;
        let local_module = match local_prefix {
            Some(prefix) if prefix.len() == 1 => Module::try_from_char(
                prefix
                    .chars()
                    .next()
                    .ok_or_else(|| StartParseError::LocalPrefix(prefix.to_owned()))?,
            )?,
            Some(prefix) => return Err(StartParseError::LocalPrefix(prefix.to_owned())),
            None => reflector_module,
        };
        Ok(Self {
            reflector_name,
            reflector_module,
            local_module,
        })
    }
}

/// Checked request retained beside its typed unsupported-profile cause.
#[derive(Debug)]
pub(super) struct StartRefusal {
    request: StartRequest,
    source: ConfigurationError,
}

impl fmt::Display for StartRefusal {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "D-STAR startup for {}", self.request.callsign)?;
        if let Some(link) = &self.request.reflector {
            write!(
                formatter,
                " to {}{} using local module {}",
                link.reflector_name, link.reflector_module, link.local_module
            )?;
        }
        write!(formatter, " refused before I/O: {}", self.source)
    }
}

impl std::error::Error for StartRefusal {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        Some(&self.source)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::AppResult;

    #[test]
    fn startup_input_is_normalized_and_checked_before_refusal() -> AppResult<()> {
        let request = StartRequest::parse(&["kq4nit", "b:ref030c"])?;
        assert_eq!(request.callsign.to_string(), "KQ4NIT");
        let link = request.reflector.as_ref().ok_or("reflector missing")?;
        assert_eq!(link.reflector_name.to_string(), "REF030");
        assert_eq!(link.local_module, Module::B);
        assert_eq!(link.reflector_module, Module::C);
        let refusal = request.refuse();
        assert!(matches!(
            refusal.source,
            ConfigurationError::UnsupportedProfile {
                profile: ConfigurationProfile::KenwoodTerminal
            }
        ));
        assert!(
            std::error::Error::source(&refusal)
                .is_some_and(|source| source.downcast_ref::<ConfigurationError>().is_some())
        );
        Ok(())
    }

    #[test]
    fn malformed_startup_values_preserve_their_typed_cause() {
        for arguments in [
            &[][..],
            &[""][..],
            &["   "][..],
            &["KQ4NIT", "REF0301"][..],
            &["KQ4NIT", "REF030C", "extra"][..],
            &["KQ4,NIT"][..],
            &["KQ4NIT\r"][..],
            &["KQ4NIT   "][..],
            &["KQ4NITé"][..],
        ] {
            assert!(StartRequest::parse(arguments).is_err(), "{arguments:?}");
        }
        assert!(matches!(
            StartRequest::parse(&["KQ4,NIT"]),
            Err(StartParseError::Identity(_))
        ));
    }
}
