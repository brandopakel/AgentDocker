//! Launch Services registration of the Mac launcher, and of nothing else.
//!
//! `open -a AgentDocker`, Spotlight and the Dock open whichever registered
//! bundle Launch Services picks for the name, and every bundle this store
//! keeps carries the same identifier and version. Hiding retained releases
//! does not keep them out: a bundle is registered wherever it sits once it
//! runs, and the launcher runs the active release from its own versioned
//! bundle, which checks in under that path. So whenever the installation
//! changes, every bundle the store keeps is forgotten except the active
//! release's own, and the visible launcher is registered last. Only bundles
//! under the installation root and the launcher are ever named, only for the
//! real home prefix, and a failure is reported, never fatal.
use super::*;
use std::path::Component;
#[cfg(all(test, target_os = "macos"))]
use std::sync::{Arc, Mutex};
use std::sync::{MutexGuard, PoisonError};
use std::time::Instant;

const LSREGISTER: &str = "/System/Library/Frameworks/CoreServices.framework/Frameworks/LaunchServices.framework/Support/lsregister";

/// How long one `lsregister` call may take.
const CALL_TIMEOUT: Duration = Duration::from_secs(15);
/// How long the calls of one change may take together.
const BUDGET: Duration = Duration::from_secs(60);
/// The inventory bound retention uses for the version store.
const MAX_ENTRIES: usize = 1000;

/// What Launch Services is asked to do with one bundle.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Registration {
    /// `lsregister -f`: register, even when the bundle looks unchanged.
    Register,
    /// `lsregister -u`: forget the path, also once nothing is there.
    Unregister,
}

impl Registration {
    fn flag(self) -> &'static str {
        match self {
            Self::Register => "-f",
            Self::Unregister => "-u",
        }
    }
}

/// What one request came to.
#[derive(Debug, PartialEq, Eq)]
pub(super) enum Outcome {
    Done,
    /// An unregistration of a path Launch Services did not know.
    Absent,
}

/// An answer to one request, and how long it took.
#[cfg(all(test, target_os = "macos"))]
type Answer =
    Box<dyn Fn(Registration, &Path, Duration) -> (Result<Outcome>, Duration) + Send + Sync>;

/// The requests a recorder has seen, in order.
#[cfg(all(test, target_os = "macos"))]
pub(super) type Calls = Arc<Mutex<Vec<(Registration, PathBuf)>>>;

/// Who answers the requests: the seam tests answer through.
pub(super) enum Registrar {
    /// The system's `lsregister`, for the real home prefix on a Mac only:
    /// a trial prefix must not register throwaway bundles on the machine.
    System,
    /// Each request answered by the test, given the time it may take, and
    /// none made; the tests that drive it are the Mac's, where bundles are
    /// registered at all.
    #[cfg(all(test, target_os = "macos"))]
    Answering(Answer),
    /// The system's `lsregister` for any prefix: a test's own bundles.
    #[cfg(all(test, target_os = "macos"))]
    Unrestricted,
}

#[cfg(all(test, target_os = "macos"))]
impl Registrar {
    /// Every request recorded and answered at once as done.
    pub(super) fn recorder() -> (Self, Calls) {
        let calls = Arc::new(Mutex::new(Vec::new()));
        let record = calls.clone();
        let answer = move |request, bundle: &Path, _| {
            record
                .lock()
                .expect("recorder lock")
                .push((request, bundle.to_owned()));
            (Ok(Outcome::Done), Duration::ZERO)
        };
        (Self::Answering(Box::new(answer)), calls)
    }
}

/// What one command told Launch Services, for its report, and how much of
/// the change's time its calls have taken: every call of the change counts,
/// publication's and pruning's as well as the settlement's.
#[derive(Default)]
pub(super) struct Settlement {
    registered: Vec<PathBuf>,
    unregistered: Vec<PathBuf>,
    errors: Vec<String>,
    spent: Duration,
    /// Requests not made once the change's time was spent.
    unmade: usize,
}

impl Settlement {
    fn record(&mut self, request: Registration, bundle: &Path, outcome: Result<Outcome>) {
        // A path ends in one list: what Launch Services was last told of it.
        let (into, from) = match request {
            Registration::Register => (&mut self.registered, &mut self.unregistered),
            Registration::Unregister => (&mut self.unregistered, &mut self.registered),
        };
        match outcome {
            Ok(Outcome::Done) => {
                from.retain(|path| path != bundle);
                if !into.iter().any(|path| path == bundle) {
                    into.push(bundle.to_owned());
                }
            }
            Ok(Outcome::Absent) => (),
            Err(error) => self.errors.push(format!("{error:#}")),
        }
    }

    /// How long the next call may take: what remains of the change's time,
    /// at most one call's bound; nothing once it is spent.
    fn allowance(&self) -> Option<Duration> {
        let remaining = BUDGET.saturating_sub(self.spent);
        (!remaining.is_zero()).then(|| remaining.min(CALL_TIMEOUT))
    }

    fn report(&mut self) -> serde_json::Value {
        let mut errors = std::mem::take(&mut self.errors);
        if self.unmade > 0 {
            errors.push(format!(
                "Launch Services took longer than {BUDGET:?}; {} requests not made",
                std::mem::take(&mut self.unmade)
            ));
        }
        json!({
            "registered": std::mem::take(&mut self.registered),
            "unregistered": std::mem::take(&mut self.unregistered),
            "errors": errors,
        })
    }
}

/// The requests that leave only the launcher answering for the name once
/// the installation has changed: forget every stored bundle except the
/// active release's own, then register the launcher, last, so it is also
/// the newest registration. The active release's bundle is left as it is:
/// it is the build the launcher runs, a running app may be using it, and it
/// checks in again whenever it starts; it is forgotten in turn once another
/// release is active. Pure: the caller reads the store.
pub(super) fn plan(
    stored: &[PathBuf],
    active: Option<&Path>,
    launcher: Option<&Path>,
) -> Vec<(Registration, PathBuf)> {
    let mut forget: Vec<PathBuf> = stored
        .iter()
        .filter(|bundle| Some(bundle.as_path()) != active && Some(bundle.as_path()) != launcher)
        .cloned()
        .collect();
    forget.sort();
    forget.dedup();
    forget
        .into_iter()
        .map(|bundle| (Registration::Unregister, bundle))
        .chain(launcher.map(|launcher| (Registration::Register, launcher.to_owned())))
        .collect()
}

/// Run `lsregister` on one bundle within `timeout`.
fn lsregister(request: Registration, bundle: &Path, timeout: Duration) -> Result<Outcome> {
    let output = command::run(
        Path::new("/"),
        &[
            LSREGISTER.to_owned(),
            request.flag().to_owned(),
            bundle.to_string_lossy().into_owned(),
        ],
        timeout,
    )
    .with_context(|| format!("lsregister {} {}", request.flag(), bundle.display()))?;
    if output.success {
        return Ok(Outcome::Done);
    }
    // kLSApplicationNotFoundErr: nothing was registered at that path.
    if request == Registration::Unregister && output.text.contains("-10814") {
        return Ok(Outcome::Absent);
    }
    let text: String = output.text.trim().chars().take(200).collect();
    bail!(
        "lsregister {} {} failed: {text}",
        request.flag(),
        bundle.display()
    )
}

impl Layout {
    /// Whether this installation tells Launch Services anything at all.
    pub(super) fn registers_bundles(&self) -> bool {
        match &self.registrar {
            Registrar::System => {
                cfg!(target_os = "macos")
                    && std::env::home_dir()
                        .and_then(|home| project::try_canonical(&home).ok())
                        .as_deref()
                        == Some(self.prefix.as_path())
                    && Path::new(LSREGISTER).is_file()
            }
            #[cfg(all(test, target_os = "macos"))]
            Registrar::Answering(_) | Registrar::Unrestricted => true,
        }
    }

    fn settlement(&self) -> MutexGuard<'_, Settlement> {
        self.told.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Ask Launch Services about one bundle this installation owns, within
    /// what remains of the change's time, and record the outcome for the
    /// command's report. Nothing for a prefix that does not register.
    pub(super) fn launch_services(&self, request: Registration, bundle: &Path) {
        if !self.registers_bundles() {
            return;
        }
        let allowance = self.settlement().allowance();
        let Some(allowance) = allowance else {
            self.settlement().unmade += 1;
            return;
        };
        let started = Instant::now();
        let (outcome, took) = match &self.registrar {
            Registrar::System => (lsregister(request, bundle, allowance), started.elapsed()),
            #[cfg(all(test, target_os = "macos"))]
            Registrar::Answering(answer) => answer(request, bundle, allowance),
            #[cfg(all(test, target_os = "macos"))]
            Registrar::Unrestricted => (lsregister(request, bundle, allowance), started.elapsed()),
        };
        let mut settlement = self.settlement();
        settlement.spent += took;
        settlement.record(request, bundle, outcome);
    }

    /// Forget a bundle the store keeps, but only while it is still
    /// physically inside the installation: `lsregister` resolves links, and
    /// through one it would forget a bundle somewhere else.
    pub(super) fn forget_stored(&self, bundle: &Path) {
        if !self.registers_bundles() {
            return;
        }
        if self.owns(bundle) {
            self.launch_services(Registration::Unregister, bundle);
        } else {
            self.settlement().errors.push(format!(
                "{} left registered: not a directory inside the installation",
                bundle.display()
            ));
        }
    }

    /// Whether every component of `bundle` below the installation root is a
    /// directory and none is a link.
    fn owns(&self, bundle: &Path) -> bool {
        let Ok(within) = bundle.strip_prefix(&self.root) else {
            return false;
        };
        let mut path = self.root.clone();
        let mut components = within.components().peekable();
        components.peek().is_some()
            && components.all(|component| {
                matches!(component, Component::Normal(_)) && {
                    path.push(component);
                    path.symlink_metadata()
                        .is_ok_and(|metadata| metadata.is_dir())
                }
            })
    }

    /// The application bundles this store keeps: each release in the version
    /// store, and each update payload extracted for installation, whatever
    /// its state. No link is followed, at any component below the root.
    pub(super) fn stored_bundles(&self) -> Result<Vec<PathBuf>> {
        let payload = Path::new("AgentDocker.app");
        let mut bundles = Vec::new();
        for (directory, within) in [
            (self.root.join("versions"), Path::new("")),
            (self.root.join("downloads"), Path::new("payload")),
        ] {
            match directory.symlink_metadata() {
                Ok(metadata) => ensure!(
                    metadata.is_dir(),
                    "{} is not a directory",
                    directory.display()
                ),
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
                Err(error) => return Err(error.into()),
            }
            let entries = std::fs::read_dir(&directory)?
                .take(MAX_ENTRIES + 1)
                .collect::<std::io::Result<Vec<_>>>()?;
            ensure!(
                entries.len() <= MAX_ENTRIES,
                "{} holds more than {MAX_ENTRIES} entries",
                directory.display()
            );
            bundles.extend(
                entries
                    .iter()
                    .map(|entry| entry.path().join(within).join(payload))
                    .filter(|bundle| self.owns(bundle)),
            );
        }
        bundles.sort();
        Ok(bundles)
    }

    /// Bring Launch Services in line with the installation as it now is,
    /// under the installation lock: after an install, update, rollback or
    /// uninstall. Nothing on a system that has no Launch Services, or for a
    /// prefix that does not register bundles.
    pub(super) fn settle_launch_services(&self) {
        if !cfg!(target_os = "macos") || !self.registers_bundles() {
            return;
        }
        // Which release is active decides which bundle stays; without that
        // answer nothing is forgotten, and the launcher is still registered.
        let stored = self.active().and_then(|active| {
            Ok((
                self.stored_bundles()?,
                active.map(|active| self.payload(&active.current)),
            ))
        });
        let (stored, active) = match stored {
            Ok(found) => found,
            Err(error) => {
                self.settlement()
                    .errors
                    .push(format!("retained bundles left registered: {error:#}"));
                (Vec::new(), None)
            }
        };
        let launcher = self
            .application
            .symlink_metadata()
            .is_ok_and(|metadata| metadata.is_dir())
            .then(|| self.application.clone());
        for (request, bundle) in plan(&stored, active.as_deref(), launcher.as_deref()) {
            match request {
                // The plan forgets stored bundles only.
                Registration::Unregister => self.forget_stored(&bundle),
                Registration::Register => self.launch_services(request, &bundle),
            }
        }
    }

    /// Everything this command told Launch Services, for its report: the
    /// `launch_services` field. Nothing where no bundle is registered.
    pub(super) fn launch_services_report(&self) -> Option<serde_json::Value> {
        (cfg!(target_os = "macos") && self.registers_bundles()).then(|| self.settlement().report())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeSet;

    /// What Launch Services would hold after `calls`, starting from `seed`.
    fn registered_after(
        seed: impl IntoIterator<Item = PathBuf>,
        calls: &[(Registration, PathBuf)],
    ) -> BTreeSet<PathBuf> {
        let mut registered: BTreeSet<PathBuf> = seed.into_iter().collect();
        for (request, bundle) in calls {
            match request {
                Registration::Register => registered.insert(bundle.clone()),
                Registration::Unregister => registered.remove(bundle),
            };
        }
        registered
    }

    #[test]
    fn the_plan_forgets_every_stored_bundle_but_the_active_one_and_registers_the_launcher_last() {
        let versions = Path::new("/home/.local/share/agentdocker/desktop/versions");
        let bundle = |id: &str| versions.join(id).join("AgentDocker.app");
        let download = Path::new(
            "/home/.local/share/agentdocker/desktop/downloads/1.2.0/payload/AgentDocker.app",
        );
        let launcher = Path::new("/Applications/AgentDocker.app");
        let stored = vec![bundle("c"), bundle("a"), download.to_owned(), bundle("b")];

        let requests = plan(&stored, Some(&bundle("b")), Some(launcher));
        assert_eq!(
            requests,
            vec![
                (Registration::Unregister, download.to_owned()),
                (Registration::Unregister, bundle("a")),
                (Registration::Unregister, bundle("c")),
                (Registration::Register, launcher.to_owned()),
            ]
        );
        // Everything once registered: only the launcher and the active
        // release's own bundle remain.
        let everything = stored.iter().cloned().chain([launcher.to_owned()]);
        assert_eq!(
            registered_after(everything, &requests),
            BTreeSet::from([bundle("b"), launcher.to_owned()])
        );

        // Uninstalled: nothing active and no launcher; everything is forgotten.
        let requests = plan(&stored, None, None);
        assert!(
            requests
                .iter()
                .all(|(request, _)| *request == Registration::Unregister)
        );
        assert!(registered_after(stored.clone(), &requests).is_empty());

        // An empty store still registers the launcher, and nothing else.
        assert_eq!(
            plan(&[], None, Some(launcher)),
            vec![(Registration::Register, launcher.to_owned())]
        );
    }

    #[test]
    fn a_prefix_that_is_not_the_home_never_runs_lsregister() {
        let tmp = tempfile::tempdir().unwrap();
        let layout = Layout::new(tmp.path().to_owned()).unwrap();
        assert!(!layout.registers_bundles());
        layout.launch_services(Registration::Register, &layout.application);
        layout.settle_launch_services();
        assert!(layout.launch_services_report().is_none());
        assert_eq!(layout.settlement().spent, Duration::ZERO);
    }

    #[test]
    fn a_settlement_reports_what_was_done_and_not_what_was_already_absent() {
        let mut settlement = Settlement::default();
        settlement.record(Registration::Unregister, Path::new("/a"), Ok(Outcome::Done));
        settlement.record(
            Registration::Unregister,
            Path::new("/b"),
            Ok(Outcome::Absent),
        );
        settlement.record(Registration::Register, Path::new("/c"), Ok(Outcome::Done));
        settlement.record(
            Registration::Unregister,
            Path::new("/d"),
            Err(anyhow::anyhow!("hung")),
        );
        // Forgotten and then registered again, as a published launcher is:
        // it is reported once, as registered.
        settlement.record(Registration::Unregister, Path::new("/c"), Ok(Outcome::Done));
        settlement.record(Registration::Register, Path::new("/c"), Ok(Outcome::Done));
        settlement.unmade = 2;
        assert_eq!(
            settlement.report(),
            json!({
                "registered": ["/c"],
                "unregistered": ["/a"],
                "errors": ["hung", "Launch Services took longer than 60s; 2 requests not made"],
            })
        );
        // A report takes what it lists.
        assert_eq!(
            settlement.report(),
            json!({"registered": [], "unregistered": [], "errors": []})
        );
    }

    #[cfg(target_os = "macos")]
    mod mac {
        use super::super::super::tests::release;
        use super::*;

        fn recording(layout: &mut Layout) -> Calls {
            let (registrar, calls) = Registrar::recorder();
            layout.registrar = registrar;
            calls
        }

        fn take(calls: &Mutex<Vec<(Registration, PathBuf)>>) -> Vec<(Registration, PathBuf)> {
            std::mem::take(&mut *calls.lock().unwrap())
        }

        /// Install, update, rollback and a reinstall of the active release,
        /// each followed by what `perform` does after it; Launch Services is
        /// modelled as having registered every bundle the store ever held,
        /// as launches and assessments do. After each, only the launcher and
        /// the active release's own bundle remain.
        #[test]
        fn install_update_and_rollback_leave_only_the_current_release_registered() {
            let tmp = tempfile::tempdir().unwrap();
            let mut layout = Layout::new(tmp.path().to_owned()).unwrap();
            layout.ensure_root().unwrap();
            let calls = recording(&mut layout);
            let first = release(&layout, "first");
            let second = release(&layout, "second");
            let third = release(&layout, "third");
            // An update extracts its payload under the root before installing.
            let download = layout.root.join("downloads/9.9.9/payload/AgentDocker.app");
            std::fs::create_dir_all(download.join("Contents")).unwrap();
            let mut ever: BTreeSet<PathBuf> = [&first, &second, &third]
                .into_iter()
                .map(|release| layout.payload(release))
                .chain([download.clone()])
                .collect();
            let mut registered = ever.clone();

            for (step, current, previous) in [
                ("install", first.clone(), None),
                ("update", second.clone(), Some(first.clone())),
                ("second update", third.clone(), Some(second.clone())),
                ("rollback", second.clone(), Some(third.clone())),
            ] {
                layout.activate(current.clone(), previous).unwrap();
                layout.settle_launch_services();
                let report = layout.launch_services_report().unwrap();
                let made = take(&calls);
                // The launcher is registered last, after every forgetting.
                assert_eq!(
                    made.last(),
                    Some(&(Registration::Register, layout.application.clone())),
                    "{step}"
                );
                assert!(
                    made.iter()
                        .all(|(_, bundle)| bundle.starts_with(&layout.root)
                            || bundle == &layout.application
                            || bundle.starts_with(layout.application.parent().unwrap())),
                    "{step}: only owned bundles are named: {made:?}"
                );
                ever.insert(layout.application.clone());
                registered = registered_after(registered.union(&ever).cloned(), &made);
                assert_eq!(
                    registered,
                    BTreeSet::from([layout.application.clone(), layout.payload(&current)]),
                    "{step}: {report}"
                );
                assert_eq!(report["errors"], json!([]), "{step}");
                assert_eq!(report["registered"], json!([layout.application]), "{step}");
            }

            // Reinstalling the active release changes nothing and keeps it so.
            let launcher = layout.prepare_launcher_bundle(&second).unwrap();
            layout.publish_launcher_bundle(launcher).unwrap();
            layout.settle_launch_services();
            let made = take(&calls);
            registered = registered_after(registered.union(&ever).cloned(), &made);
            assert_eq!(
                registered,
                BTreeSet::from([layout.application.clone(), layout.payload(&second)])
            );
        }

        #[test]
        fn publishing_forgets_the_replaced_launcher_before_registering_the_new_one() {
            let tmp = tempfile::tempdir().unwrap();
            let mut layout = Layout::new(tmp.path().to_owned()).unwrap();
            layout.ensure_root().unwrap();
            let calls = recording(&mut layout);
            let first = release(&layout, "first");
            let second = release(&layout, "second");
            layout.activate(first.clone(), None).unwrap();
            take(&calls);
            layout.activate(second, Some(first)).unwrap();
            let made = take(&calls);
            let parent = layout.application.parent().unwrap();
            // Before the exchange, the visible app; after it, the replaced
            // copy in the private staging directory; then the new app.
            assert_eq!(made.len(), 3, "{made:?}");
            assert_eq!(
                made[0],
                (Registration::Unregister, layout.application.clone())
            );
            assert_eq!(made[1].0, Registration::Unregister);
            assert!(
                made[1].1.starts_with(parent)
                    && made[1].1 != layout.application
                    && made[1].1.ends_with("AgentDocker.app"),
                "{made:?}"
            );
            assert!(!made[1].1.exists(), "the staging copy is gone afterwards");
            assert_eq!(
                made[2],
                (Registration::Register, layout.application.clone())
            );
        }

        /// A link anywhere below the root, not only at the bundle itself,
        /// keeps its target out of the inventory and out of every request.
        #[test]
        fn no_link_below_the_root_reaches_launch_services() {
            use std::os::unix::fs::symlink;
            let tmp = tempfile::tempdir().unwrap();
            let mut layout = Layout::new(tmp.path().to_owned()).unwrap();
            layout.ensure_root().unwrap();
            let calls = recording(&mut layout);
            let outside = tempfile::tempdir().unwrap();
            let outside = outside.path();
            std::fs::create_dir_all(outside.join("payload/AgentDocker.app")).unwrap();
            std::fs::create_dir_all(outside.join("AgentDocker.app")).unwrap();
            let root = layout.root.clone();
            let real = root.join("versions/real/AgentDocker.app");
            let download = root.join("downloads/9.9.9/payload/AgentDocker.app");
            std::fs::create_dir_all(&real).unwrap();
            std::fs::create_dir_all(&download).unwrap();
            // An entry of each store, an update's payload directory and a
            // bundle itself, each a link out of the root.
            symlink(outside, root.join("versions/entry")).unwrap();
            symlink(outside, root.join("downloads/entry")).unwrap();
            std::fs::create_dir_all(root.join("downloads/inner")).unwrap();
            symlink(
                outside.join("payload"),
                root.join("downloads/inner/payload"),
            )
            .unwrap();
            std::fs::create_dir_all(root.join("versions/bundle")).unwrap();
            symlink(
                outside.join("AgentDocker.app"),
                root.join("versions/bundle/AgentDocker.app"),
            )
            .unwrap();
            assert_eq!(
                layout.stored_bundles().unwrap(),
                [download.clone(), real.clone()]
            );
            layout.settle_launch_services();
            let made = take(&calls);
            assert_eq!(
                made,
                [
                    (Registration::Unregister, download.clone()),
                    (Registration::Unregister, real.clone()),
                ]
            );

            // Pruning names a bundle directly, and is refused the same way.
            layout.forget_stored(&root.join("downloads/entry/payload/AgentDocker.app"));
            assert!(take(&calls).is_empty());
            let report = layout.launch_services_report().unwrap();
            assert_eq!(report["errors"].as_array().unwrap().len(), 1, "{report}");

            // A store that is itself a link is not read at all: nothing is
            // forgotten, and the inventory's refusal is reported.
            std::fs::remove_dir_all(root.join("downloads")).unwrap();
            symlink(outside, root.join("downloads")).unwrap();
            assert!(layout.stored_bundles().is_err());
            layout.settle_launch_services();
            assert!(take(&calls).is_empty());
            let report = layout.launch_services_report().unwrap();
            assert!(
                report["errors"][0]
                    .as_str()
                    .unwrap()
                    .starts_with("retained bundles left registered"),
                "{report}"
            );
        }

        /// The calls of one change share one budget, each capped at what
        /// remains of it, whoever makes them: a stalled `lsregister` holds
        /// the installation lock for the budget at most, and the requests
        /// past it are reported as not made.
        #[test]
        fn the_calls_of_one_change_share_one_budget() {
            let tmp = tempfile::tempdir().unwrap();
            let mut layout = Layout::new(tmp.path().to_owned()).unwrap();
            layout.ensure_root().unwrap();
            for id in 0..10 {
                std::fs::create_dir_all(
                    layout
                        .root
                        .join(format!("versions/{id:02}/AgentDocker.app")),
                )
                .unwrap();
            }
            let given = Arc::new(Mutex::new(Vec::new()));
            let seen = given.clone();
            layout.registrar = Registrar::Answering(Box::new(move |_, _, allowance| {
                let mut seen = seen.lock().unwrap();
                seen.push(allowance);
                if seen.len() == 1 {
                    return (Ok(Outcome::Done), Duration::from_secs(10));
                }
                // Stalled, and ended at its allowance.
                (Err(anyhow::anyhow!("timed out")), allowance)
            }));
            // A pruned build is forgotten first, in the same change.
            layout.forget_stored(&layout.root.join("versions/00/AgentDocker.app"));
            layout.settle_launch_services();
            let given = given.lock().unwrap().clone();
            let secs = Duration::from_secs;
            assert_eq!(given, [secs(15), secs(15), secs(15), secs(15), secs(5)]);
            assert_eq!(layout.settlement().spent, BUDGET);
            let report = layout.launch_services_report().unwrap();
            let errors = report["errors"].as_array().unwrap();
            assert_eq!(errors.len(), 5, "{report}");
            assert_eq!(
                errors[4],
                json!("Launch Services took longer than 60s; 6 requests not made")
            );
            assert_eq!(report["unregistered"].as_array().unwrap().len(), 1);
        }

        /// What publishing tells Launch Services joins the change's report,
        /// a failure to forget the replaced staging copy included, while the
        /// install itself succeeds.
        #[test]
        fn a_failure_while_publishing_is_reported_with_the_settlement() {
            let tmp = tempfile::tempdir().unwrap();
            let mut layout = Layout::new(tmp.path().to_owned()).unwrap();
            layout.ensure_root().unwrap();
            let application = layout.application.clone();
            layout.registrar = Registrar::Answering(Box::new(move |request, bundle, _| {
                let staged = request == Registration::Unregister && bundle != application;
                let staged = staged && !bundle.to_string_lossy().contains("/versions/");
                if staged {
                    return (Err(anyhow::anyhow!("staging copy refused")), Duration::ZERO);
                }
                (Ok(Outcome::Done), Duration::ZERO)
            }));
            let first = release(&layout, "first");
            let second = release(&layout, "second");
            layout.activate(first.clone(), None).unwrap();
            layout.activate(second, Some(first.clone())).unwrap();
            layout.settle_launch_services();
            let report = layout.launch_services_report().unwrap();
            assert_eq!(
                report["errors"],
                json!(["staging copy refused"]),
                "{report}"
            );
            assert_eq!(report["registered"], json!([layout.application]));
            assert_eq!(report["unregistered"], json!([layout.payload(&first)]));
        }
    }

    /// The real `lsregister`, on throwaway bundles in a temporary directory
    /// only: the requests the plan makes leave the launcher registered and
    /// no retained release, after an install and after a rollback. The
    /// bundles have a name and identifier of their own, never AgentDocker's,
    /// so `open -a AgentDocker` cannot find them meanwhile, and each is
    /// forgotten again however the test ends.
    #[cfg(target_os = "macos")]
    #[test]
    fn real_launch_services_forgets_retained_bundles_and_keeps_the_launcher() {
        if !Path::new(LSREGISTER).is_file() {
            eprintln!("no lsregister on this system; skipped");
            return;
        }
        let tmp = tempfile::tempdir().unwrap();
        let root = project::try_canonical(tmp.path()).unwrap();
        let tag = format!("{:x}", Sha256::digest(root.to_string_lossy().as_bytes()));
        let name = format!("ADProbe{}", &tag[..12]);
        let bundle = |directory: PathBuf| {
            let bundle = directory.join(format!("{name}.app"));
            std::fs::create_dir_all(bundle.join("Contents/MacOS")).unwrap();
            std::fs::write(
                bundle.join("Contents/Info.plist"),
                format!(
                    "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<plist version=\"1.0\"><dict>\
                     <key>CFBundleIdentifier</key><string>dev.agentdocker.test.{tag}</string>\
                     <key>CFBundleName</key><string>{name}</string>\
                     <key>CFBundleExecutable</key><string>probe</string>\
                     <key>CFBundlePackageType</key><string>APPL</string>\
                     <key>CFBundleVersion</key><string>1.0</string></dict></plist>\n"
                ),
            )
            .unwrap();
            std::fs::write(bundle.join("Contents/MacOS/probe"), "#!/bin/sh\nexit 0\n").unwrap();
            bundle
        };
        let versions: Vec<PathBuf> = ["a", "b", "c"]
            .into_iter()
            .map(|id| bundle(root.join("versions").join(id)))
            .collect();
        let launcher = bundle(root.join("Applications"));
        let every: Vec<PathBuf> = versions.iter().cloned().chain([launcher.clone()]).collect();

        struct Forget(Vec<PathBuf>);
        impl Drop for Forget {
            fn drop(&mut self) {
                for bundle in &self.0 {
                    let _ = lsregister(Registration::Unregister, bundle, CALL_TIMEOUT);
                }
            }
        }
        let _forget = Forget(every.clone());

        let registered = || -> BTreeSet<PathBuf> {
            let output = std::process::Command::new(LSREGISTER)
                .args(["-dump", "Bundle"])
                .output()
                .unwrap();
            let text = String::from_utf8_lossy(&output.stdout);
            every
                .iter()
                .filter(|bundle| {
                    let line = format!("{} (0x", bundle.display());
                    text.lines().any(|row| {
                        row.starts_with("path:")
                            && row
                                .trim_start_matches("path:")
                                .trim_start()
                                .starts_with(&line)
                    })
                })
                .cloned()
                .collect()
        };
        // Launch Services has seen them all, as it has every release that
        // ever ran.
        for bundle in &every {
            assert_eq!(
                lsregister(Registration::Register, bundle, CALL_TIMEOUT).unwrap(),
                Outcome::Done
            );
        }
        let seen = registered();
        if seen.len() != every.len() {
            eprintln!("Launch Services does not answer here ({seen:?}); skipped");
            return;
        }
        let tmp_layout = tempfile::tempdir().unwrap();
        let mut layout = Layout::new(tmp_layout.path().to_owned()).unwrap();
        layout.registrar = Registrar::Unrestricted;
        let apply = |requests: Vec<(Registration, PathBuf)>| {
            for (request, bundle) in requests {
                layout.launch_services(request, &bundle);
            }
            let report = layout.launch_services_report().unwrap();
            assert_eq!(report["errors"], json!([]), "{report}");
        };

        // Installed `c`, retaining `a` and `b`: the launcher answers, with
        // the active release's own bundle, which it runs.
        apply(plan(&versions, Some(&versions[2]), Some(&launcher)));
        assert_eq!(
            registered(),
            BTreeSet::from([versions[2].clone(), launcher.clone()])
        );
        // Rolled back to `b`, with `a` pruned from the disk meanwhile (an
        // unregistration does not need the bundle): `c` is retained and
        // forgotten, and `b` stays forgotten until it runs.
        std::fs::remove_dir_all(versions[0].parent().unwrap()).unwrap();
        apply(plan(&versions, Some(&versions[1]), Some(&launcher)));
        assert_eq!(registered(), BTreeSet::from([launcher.clone()]));
        // `b` runs, and its launch registers it, as any launch does; the
        // next update forgets it once it is no longer active.
        assert_eq!(
            lsregister(Registration::Register, &versions[1], CALL_TIMEOUT).unwrap(),
            Outcome::Done
        );
        apply(plan(&versions, Some(&versions[2]), Some(&launcher)));
        assert_eq!(registered(), BTreeSet::from([launcher.clone()]));
    }
}
