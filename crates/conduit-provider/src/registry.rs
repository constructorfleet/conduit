//! Runtime lookup of providers by name.
//!
//! Pipeline graphs refer to providers by string — `"whisper"`, `"piper"` —
//! and the registry is what turns those strings into implementations. One
//! registry holds one capability, e.g. `Registry<dyn SpeechToText>`.
//!
//! A [`Registry`] of any capability strips down to the [`RegistryHandle`]
//! interface, so a collection of registries — a provider bundle — can enumerate
//! every capability uniformly without knowing each registry's type.

use std::any::Any;
use std::collections::BTreeMap;
use std::sync::Arc;

use conduit_core::{Error, Result};
use serde::{Deserialize, Serialize};

use crate::descriptor::Descriptor;
use crate::Provider;

/// A capability a provider supplies, and the dimension a bundle is indexed by.
///
/// Adding a capability is data: a new variant (and a `name`, and a slot on
/// whichever bundle constructs one [`Registry`] per capability). Nothing that
/// *enumerates* capabilities — a registry, a bundle, its debug output — changes
/// structure to accommodate it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Capability {
    /// Speech recognition.
    Stt,
    /// Language model reasoning.
    Llm,
    /// Speech synthesis.
    Tts,
    /// Rewriting an utterance before it is rendered.
    Transform,
    /// Tool invocation.
    Tool,
    /// Long-term memory.
    Memory,
    /// Wake word detection.
    Wake,
    /// Speaker identification.
    SpeakerId,
}

impl Capability {
    /// Every capability, in registry order.
    ///
    /// The single list that must grow when a capability is added. It lives on
    /// the enum rather than in a bundle so that a bundle builds itself from
    /// this rather than naming each capability on its own.
    pub const ALL: [Self; 8] = [
        Self::Stt,
        Self::Llm,
        Self::Tts,
        Self::Transform,
        Self::Tool,
        Self::Memory,
        Self::Wake,
        Self::SpeakerId,
    ];

    /// The word this capability is written as in diagnostics and listings.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Stt => "stt",
            Self::Llm => "llm",
            Self::Tts => "tts",
            Self::Transform => "transform",
            Self::Tool => "tool",
            Self::Memory => "memory",
            Self::Wake => "wake",
            Self::SpeakerId => "speaker_id",
        }
    }
}

/// What a registry of any capability looks like through a type-erasing handle.
///
/// A runtime bundle holds one registry per [`Capability`] behind a boxed
/// handle, so it can enumerate a wake store next to a recognizer without
/// caring which is which, and hand the caller the typed registry back through
/// `Any`.
pub trait RegistryHandle: Send + Sync {
    /// Registration keys, in order.
    fn names(&self) -> Vec<String>;

    /// Every registered provider's [`Descriptor`], paired with the key it is
    /// registered under, in key order.
    ///
    /// What lets a status layer or an operator screen render any provider of
    /// any capability without knowing which capability it is: the key is the
    /// selector a pipeline names, and everything shown about the provider —
    /// its identity, its label, what it can do, what it accepts — comes from
    /// the descriptor.
    fn descriptors(&self) -> Vec<(String, Descriptor)>;

    /// Number of registered providers.
    fn len(&self) -> usize;

    /// Whether nothing is registered.
    fn is_empty(&self) -> bool;

    /// The typed [`Registry`] behind this handle, for downcasting.
    fn as_any(&self) -> &dyn Any;

    /// The typed [`Registry`] behind this handle, mutably, for downcasting.
    fn as_any_mut(&mut self) -> &mut dyn Any;
}

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

impl<T: Provider + ?Sized> Registry<T> {
    /// Registers `provider` under its own [`Descriptor::id`].
    ///
    /// The shape to reach for when the deployment has no separate selector to
    /// give a provider: the key is then the identity the provider states,
    /// rather than a string the caller repeats and can get wrong. A deployment
    /// that registers one implementation twice keys them with
    /// [`Registry::insert`] instead.
    pub fn register(&mut self, provider: Arc<T>) -> Option<Arc<T>> {
        let id = provider.descriptor().id.clone();
        self.insert(id, provider)
    }

    /// Every registered provider's [`Descriptor`], keyed by its selector.
    pub fn descriptors(&self) -> impl Iterator<Item = (&str, &Descriptor)> {
        self.providers.iter().map(|(key, provider)| (key.as_str(), provider.descriptor()))
    }
}

impl<T: Provider + ?Sized> RegistryHandle for Registry<T> {
    fn names(&self) -> Vec<String> {
        Registry::names(self).map(str::to_owned).collect()
    }

    fn descriptors(&self) -> Vec<(String, Descriptor)> {
        Registry::descriptors(self)
            .map(|(key, descriptor)| (key.to_owned(), descriptor.clone()))
            .collect()
    }

    fn len(&self) -> usize {
        Registry::len(self)
    }

    fn is_empty(&self) -> bool {
        Registry::is_empty(self)
    }

    fn as_any(&self) -> &dyn Any {
        self
    }

    fn as_any_mut(&mut self) -> &mut dyn Any {
        self
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

    trait Speaks: Provider {
        fn greet(&self) -> String;
    }

    struct FixedSpeaker(Descriptor);

    impl FixedSpeaker {
        fn new(id: &str) -> Self {
            Self(Descriptor::new(id, Capability::Wake).with_label("Fixed"))
        }
    }

    impl Provider for FixedSpeaker {
        fn descriptor(&self) -> &Descriptor {
            &self.0
        }
    }

    impl Speaks for FixedSpeaker {
        fn greet(&self) -> String {
            self.0.id.clone()
        }
    }

    #[test]
    fn every_capability_is_listed_once() {
        assert_eq!(Capability::ALL.len(), 8);
        assert_eq!(
            Capability::ALL.iter().copied().collect::<std::collections::BTreeSet<_>>().len(),
            8,
            "no capability is listed twice"
        );
    }

    #[test]
    fn a_capability_writes_as_a_stable_word() {
        assert_eq!(Capability::Wake.as_str(), "wake");
        assert_eq!(Capability::SpeakerId.as_str(), "speaker_id");
        assert_eq!(Capability::Memory.as_str(), "memory");
    }

    #[test]
    fn a_registry_is_usable_as_a_type_erased_handle() {
        let mut registry = Registry::<dyn Speaks>::new();
        registry
            .insert("okay-nabu", Arc::new(FixedSpeaker::new("okay-nabu")) as Arc<dyn Speaks>);
        let handle: Box<dyn RegistryHandle> = Box::new(registry);

        assert_eq!(handle.names(), ["okay-nabu"]);
        assert_eq!(handle.len(), 1);
        assert!(!handle.is_empty());

        let typed =
            handle.as_any().downcast_ref::<Registry<dyn Speaks>>().expect("same concrete type");
        assert_eq!(typed.require("okay-nabu").expect("registered").greet(), "okay-nabu");
    }

    #[test]
    fn a_handles_registry_downcasts_mutably_for_further_registration() {
        let mut handle: Box<dyn RegistryHandle> = Box::new(Registry::<dyn Speaks>::new());
        handle
            .as_any_mut()
            .downcast_mut::<Registry<dyn Speaks>>()
            .expect("same concrete type")
            .insert("okay-nabu", Arc::new(FixedSpeaker::new("okay-nabu")) as Arc<dyn Speaks>);

        assert_eq!(handle.names(), ["okay-nabu"]);
    }

    #[test]
    fn a_provider_registers_under_the_identity_it_states() {
        // The key used to be a string the caller repeated; a provider that
        // states its own identity is registered under it.
        let mut registry = Registry::<dyn Speaks>::new();
        registry.register(Arc::new(FixedSpeaker::new("okay-nabu")) as Arc<dyn Speaks>);

        assert_eq!(registry.names().collect::<Vec<_>>(), ["okay-nabu"]);
    }

    #[test]
    fn a_registry_reports_each_providers_descriptor_beside_its_selector() {
        // A deployment may register one implementation under a selector of its
        // own choosing; the key and the identity are then different strings,
        // and a listing has to show both.
        let mut registry = Registry::<dyn Speaks>::new();
        registry
            .insert("front-door", Arc::new(FixedSpeaker::new("okay-nabu")) as Arc<dyn Speaks>);
        let handle: Box<dyn RegistryHandle> = Box::new(registry);

        let descriptors = handle.descriptors();
        let (key, descriptor) = descriptors.first().expect("one registration");
        assert_eq!(key, "front-door", "the selector a pipeline names");
        assert_eq!(descriptor.id, "okay-nabu", "what the provider calls itself");
        assert_eq!(descriptor.label, "Fixed", "what an operator screen shows");
        assert_eq!(descriptor.capability, Capability::Wake);
    }
}
