//! Executable transport exercise: emits observations through an inherited pipe.
//! It creates no metrics files and does not measure a consuming product.
use ui_observation_protocol::{ElementKey, ObservationBody, StateRevision};
use ui_observation_runtime::{ObservationRuntime, RecordStatus};

fn main() {
    let runtime = ObservationRuntime::from_inherited_v1();
    let window = ElementKey::parse("window.main").unwrap();
    for frame in 0..32 {
        assert_eq!(
            runtime.record(
                frame,
                window.clone(),
                ObservationBody::StateRevision(StateRevision {
                    target: ElementKey::parse("example.identity").unwrap(),
                    revision: frame,
                    cause_action_sequence: None,
                })
            ),
            RecordStatus::Recorded
        );
    }
    assert_eq!(runtime.finish(31, window), RecordStatus::Recorded);
}
