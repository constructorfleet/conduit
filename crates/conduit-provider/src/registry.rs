//! Runtime lookup of providers by name.
//!
//! Pipeline graphs refer to providers by string — `"whisper"`, `"piper"` —
//! and the registry is what turns those strings into implementations. One
//! registry holds one capability, e.g. `Registry<dyn SpeechToText>`.

use std::collections::BTreeMap;
use std::sync::Arc;

use conduit_core::{Error, Result};

/// A name-to-implementation map for one provider capability.
///
/// Entries are stored in name order so listings are stable in the UI and in
/// tests.
#[derive(Debug)]
pub struct Registry<T: ?Sized> {
    providers: BTreeMap<String, Arc<T>>,
    default: Option<String>,
}

impl<T: ?Sized> Registry<T> {
    /// Creates an empty registry.
    #[must_use]
    pub fn new() -> Self {
        Self { providers: BTreeMap::new(), default: None }
    }

    /// Registers `provider` under `name`, returning any implementation it
    /// displaced.
    ///
    /// The first registration also becomes the default, so a single-provider
    /// deployment needs no further configuration.
    pub fn insert(&mut self, name: impl Into<String>, provider: Arc<T>) -> Option<Arc<T>> {
        let name = name.into();
        if self.default.is_none() {
            self.default = Some(name.clone());
        }
        self.providers.insert(name, provider)
    }

    /// Looks up a provider by name.
    #[must_use]
    pub fn get(&self, name: &str) -> Option<Arc<T>> {
        self.providers.get(name).map(Arc::clone)
    }

    /// Looks up a provider by name, failing if it is not registered.
    ///
    /// # Errors
    ///
    /// Returns [`Error::UnknownProvider`] when nothing is registered under
    /// `name`.
    pub fn require(&self, name: &str) -> Result<Arc<T>> {
        self.get(name).ok_or_else(|| Error::UnknownProvider(name.to_owned()))
    }

    /// Resolves an optional name against the registry, falling back to the
    /// default when `name` is `None`.
    ///
    /// This is what optional provider selection uses: callers may pin a
    /// provider or leave the choice to the deployment.
    ///
    /// # Errors
    ///
    /// Returns [`Error::UnknownProvider`] when the named provider is missing,
    /// or [`Error::Config`] when no name was given and no default exists.
    pub fn resolve(&self, name: Option<&str>) -> Result<Arc<T>> {
        match name {
            Some(name) => self.require(name),
            None => {
                let default = self
                    .default
                    .as_deref()
                    .ok_or_else(|| Error::Config("no providers registered".to_owned()))?;
                self.require(default)
            }
        }
    }

    /// Chooses which provider [`Registry::resolve`] falls back to.
    ///
    /// # Errors
    ///
    /// Returns [`Error::UnknownProvider`] if `name` is not registered.
    pub fn set_default(&mut self, name: impl Into<String>) -> Result<()> {
        let name = name.into();
        if !self.providers.contains_key(&name) {
            return Err(Error::UnknownProvider(name));
        }
        self.default = Some(name);
        Ok(())
    }

    /// Name of the current default, if any.
    #[must_use]
    pub fn default_name(&self) -> Option<&str> {
        self.default.as_deref()
    }

    /// Registered names, in order.
    pub fn names(&self) -> impl Iterator<Item = &str> {
        self.providers.keys().map(String::as_str)
    }

    /// Number of registered providers.
    #[must_use]
    pub fn len(&self) -> usize {
        self.providers.len()
    }

    /// Whether nothing is registered.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.providers.is_empty()
    }
}

impl<T: ?Sized> Default for Registry<T> {
    fn default() -> Self {
        Self::new()
    }
}

impl<T: ?Sized> Clone for Registry<T> {
    fn clone(&self) -> Self {
        Self { providers: self.providers.clone(), default: self.default.clone() }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    trait Greeter: Send + Sync {
        fn greet(&self) -> String;
    }

    struct Fixed(&'static str);

    impl Greeter for Fixed {
        fn greet(&self) -> String {
            self.0.to_owned()
        }
    }

    fn registry() -> Registry<dyn Greeter> {
        let mut registry = Registry::<dyn Greeter>::new();
        registry.insert("piper", Arc::new(Fixed("piper")) as Arc<dyn Greeter>);
        registry.insert("elevenlabs", Arc::new(Fixed("elevenlabs")) as Arc<dyn Greeter>);
        registry
    }

    #[test]
    fn first_registration_becomes_the_default() {
        let registry = registry();
        assert_eq!(registry.default_name(), Some("piper"));
        assert_eq!(registry.resolve(None).expect("default").greet(), "piper");
    }

    #[test]
    fn explicit_names_win_over_the_default() {
        let registry = registry();
        assert_eq!(registry.resolve(Some("elevenlabs")).expect("named").greet(), "elevenlabs");
    }

    #[test]
    fn names_are_listed_in_order() {
        assert_eq!(registry().names().collect::<Vec<_>>(), ["elevenlabs", "piper"]);
    }

    #[test]
    fn unknown_providers_are_rejected() {
        let registry = registry();
        let Err(error) = registry.require("azure") else {
            panic!("expected `azure` to be unregistered");
        };
        assert!(matches!(error, Error::UnknownProvider(name) if name == "azure"));
    }

    #[test]
    fn default_cannot_be_set_to_an_unregistered_provider() {
        let mut registry = registry();
        assert!(registry.set_default("azure").is_err());
        assert_eq!(registry.default_name(), Some("piper"));
    }

    #[test]
    fn resolving_an_empty_registry_is_a_config_error() {
        let registry = Registry::<dyn Greeter>::new();
        assert!(matches!(registry.resolve(None), Err(Error::Config(_))));
    }
}
