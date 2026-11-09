use std::fmt;
use std::time::Instant;

use bctl_cdp::protocol::{Command, Effect, Empty, GetTargets, TargetId, TargetInfo};
use bctl_cdp::{CallError, Connection, Generation, Scope};
use miniserde::{Deserialize, Serialize};

use crate::{COMMAND_DEADLINE, SafeUrl, TargetScope};

/// A page target: one tab or popup. Its CDP `targetId` is its identity for its whole life.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PageTarget {
    pub id: TargetId,
    pub title: String,
    pub url: SafeUrl,
}

impl PageTarget {
    fn from_info(info: TargetInfo) -> Option<PageTarget> {
        (info.kind == "page").then(|| PageTarget { id: info.target_id, title: info.title, url: SafeUrl::new(&info.url) })
    }
}

/// The target a session's native commands act on, as remembered for one browser generation.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Binding {
    pub target: TargetId,
    /// The target's URL when it was selected, kept only to describe it once it is gone.
    pub url: SafeUrl,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BindingStoreError(pub String);

impl fmt::Display for BindingStoreError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

/// Durable storage for one session's binding. Every call names the generation it is about; a store must
/// never return a binding saved for another generation.
pub trait BindingStore {
    fn load(&self, generation: Generation) -> Result<Option<Binding>, BindingStoreError>;
    fn save(&self, generation: Generation, binding: &Binding) -> Result<(), BindingStoreError>;
    /// Forgets the binding only if it is still `target`.
    fn release(&self, generation: Generation, target: &TargetId) -> Result<(), BindingStoreError>;
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Selection {
    Unselected,
    Selected(TargetId),
    /// The bound target no longer exists. Nothing replaces it until a target is selected explicitly.
    Gone(Binding),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Listing {
    pub targets: Vec<PageTarget>,
    pub selection: Selection,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum TabError {
    /// The selected target is gone. `remaining` is every page target that still exists.
    TargetGone { binding: Binding, remaining: Vec<PageTarget> },
    NotSelected { targets: Vec<PageTarget> },
    NoSuchTarget { target: TargetId, targets: Vec<PageTarget> },
    /// Closing the only page target would end the browser.
    LastTarget { target: TargetId },
    Cdp(CallError),
    Store(BindingStoreError),
}

impl fmt::Display for TabError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            TabError::TargetGone { binding, remaining } => {
                write!(f, "the selected target {} ({}) is gone; {} other page targets remain", binding.target, binding.url, remaining.len())
            }
            TabError::NotSelected { targets } => write!(f, "no target is selected; {} page targets exist", targets.len()),
            TabError::NoSuchTarget { target, .. } => write!(f, "no page target {target}"),
            TabError::LastTarget { target } => write!(f, "{target} is the only page target; closing it would end the browser"),
            TabError::Cdp(error) => error.fmt(f),
            TabError::Store(error) => write!(f, "cannot record the selected target: {error}"),
        }
    }
}

impl std::error::Error for TabError {}

impl From<CallError> for TabError {
    fn from(error: CallError) -> TabError {
        TabError::Cdp(error)
    }
}

impl From<BindingStoreError> for TabError {
    fn from(error: BindingStoreError) -> TabError {
        TabError::Store(error)
    }
}

#[derive(Serialize, Debug)]
struct CreateTarget {
    url: String,
}

#[derive(Deserialize, Debug)]
struct Created {
    #[serde(rename = "targetId")]
    target_id: TargetId,
}

impl Command for CreateTarget {
    const METHOD: &'static str = "Target.createTarget";
    const EFFECT: Effect = Effect::Mutating;
    type Returns = Created;
}

#[derive(Serialize, Debug)]
struct CloseTarget {
    #[serde(rename = "targetId")]
    target_id: TargetId,
}

impl Command for CloseTarget {
    const METHOD: &'static str = "Target.closeTarget";
    const EFFECT: Effect = Effect::Mutating;
    type Returns = Empty;
}

/// The page targets of one browser generation and the one its native commands act on. The selection moves
/// only when a caller selects, opens, or closes a target; a popup, a tab the user activates, or a target that
/// disappears never moves it.
pub struct Tabs<'a, S> {
    connection: &'a Connection,
    store: &'a S,
}

impl<'a, S: BindingStore> Tabs<'a, S> {
    pub fn new(connection: &'a Connection, store: &'a S) -> Tabs<'a, S> {
        Tabs { connection, store }
    }

    fn generation(&self) -> Generation {
        self.connection.generation()
    }

    async fn call<C: Command>(&self, command: &C) -> Result<C::Returns, CallError> {
        self.connection.call(Scope::Browser, command, Instant::now() + COMMAND_DEADLINE).await
    }

    pub async fn pages(&self) -> Result<Vec<PageTarget>, TabError> {
        let targets = self.call(&GetTargets {}).await?;
        Ok(targets.target_infos.into_iter().filter_map(PageTarget::from_info).collect())
    }

    fn selection(&self, targets: &[PageTarget]) -> Result<Selection, TabError> {
        Ok(match self.store.load(self.generation())? {
            None => Selection::Unselected,
            Some(binding) if targets.iter().any(|t| t.id == binding.target) => Selection::Selected(binding.target),
            Some(binding) => Selection::Gone(binding),
        })
    }

    pub async fn list(&self) -> Result<Listing, TabError> {
        let targets = self.pages().await?;
        let selection = self.selection(&targets)?;
        Ok(Listing { targets, selection })
    }

    /// The target native commands act on, if it still exists.
    pub async fn selected(&self) -> Result<TargetScope, TabError> {
        let Listing { targets, selection } = self.list().await?;
        match selection {
            Selection::Selected(target) => Ok(TargetScope { generation: self.generation(), target }),
            Selection::Unselected => Err(TabError::NotSelected { targets }),
            Selection::Gone(binding) => Err(TabError::TargetGone { binding, remaining: targets }),
        }
    }

    pub async fn select(&self, target: &TargetId) -> Result<Listing, TabError> {
        let targets = self.pages().await?;
        let Some(page) = targets.iter().find(|t| t.id == *target) else {
            return Err(TabError::NoSuchTarget { target: target.clone(), targets });
        };
        self.store.save(self.generation(), &Binding { target: page.id.clone(), url: page.url.clone() })?;
        Ok(Listing { selection: Selection::Selected(page.id.clone()), targets })
    }

    /// Opens a page target at `url` and selects it.
    pub async fn open(&self, url: &str) -> Result<Listing, TabError> {
        let created = self.call(&CreateTarget { url: url.to_string() }).await?;
        self.store.save(self.generation(), &Binding { target: created.target_id.clone(), url: SafeUrl::new(url) })?;
        let targets = self.pages().await?;
        Ok(Listing { selection: Selection::Selected(created.target_id), targets })
    }

    /// Closes a page target. Closing the selected one leaves nothing selected.
    pub async fn close(&self, target: &TargetId) -> Result<Listing, TabError> {
        let mut targets = self.pages().await?;
        if !targets.iter().any(|t| t.id == *target) {
            return Err(TabError::NoSuchTarget { target: target.clone(), targets });
        }
        if targets.len() == 1 {
            return Err(TabError::LastTarget { target: target.clone() });
        }
        self.call(&CloseTarget { target_id: target.clone() }).await?;
        self.store.release(self.generation(), target)?;
        targets.retain(|t| t.id != *target);
        let selection = self.selection(&targets)?;
        Ok(Listing { targets, selection })
    }
}
