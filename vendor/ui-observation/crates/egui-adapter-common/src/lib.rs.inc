use std::cell::RefCell;
use ui_observation_protocol::{
    Content, ElementKey, ElementObservation, ElementSpec, Fixed6, ObservationBody, Rect,
    RegionObservation, RegionSpec, Role, States, WindowObservation,
};
use ui_observation_runtime::ObservationRuntime;

const COMPLETION_HOOK_ID: &str = "ui.observation.completion-hook.v1";
const DEFER_COMPLETION_ID: &str = "ui.observation.render-output.pending.v1";
const RUNTIME_ID: &str = "ui.observation.runtime.v1";
const DEFAULT_WINDOW_KEY: &str = "window.main";

thread_local! {
    // ScrollAreaOutput does not retain its Context. Installation captures it for
    // the current render thread so the contract's output.observe_region(spec)
    // surface remains possible without wrapping or reconstructing the output.
    static CURRENT_CONTEXT: RefCell<Option<egui::Context>> = const { RefCell::new(None) };
}

pub trait ContextObservationExt {
    /// Installs a runtime in context-local data and selects this context for
    /// scroll-output observations on the current render thread.
    fn install_observation_runtime(&self, runtime: ObservationRuntime);

    /// Removes observation from this context without affecting rendering.
    fn remove_observation_runtime(&self);

    /// Records the current native-viewport-facing window geometry for this
    /// frame. This remains an application fact; native presentation must still
    /// be corroborated externally.
    fn observe_window(&self, public_title: &str);

    /// Enqueues a clean stream end using the context's current pass number.
    fn finish_observation(&self);
}

impl ContextObservationExt for egui::Context {
    fn install_observation_runtime(&self, installed_runtime: ObservationRuntime) {
        self.data_mut(|data| data.insert_temp(egui::Id::new(RUNTIME_ID), installed_runtime));
        CURRENT_CONTEXT.with(|slot| *slot.borrow_mut() = Some(self.clone()));
        let installed=self.data_mut(|data| {
            let id=egui::Id::new(COMPLETION_HOOK_ID);
            let already=data.get_temp::<bool>(id).unwrap_or(false);
            data.insert_temp(id,true); already
        });
        if !installed {
            self.on_end_pass("ui-observation-frame-complete",std::sync::Arc::new(|context| {
                if let Some(runtime)=runtime(context) {
                    if context.data(|data|data.get_temp::<bool>(egui::Id::new(DEFER_COMPLETION_ID)).unwrap_or(false)){return;}
                    let frame=context.cumulative_pass_nr();
                    let _=runtime.record(frame,default_window_key(),ObservationBody::StateRevision(ui_observation_protocol::StateRevision {
                        target:ElementKey::parse(ui_observation_protocol::FRAME_COMPLETE_KEY).expect("frame completion key"), revision:frame, cause_action_sequence:None,
                    }));
                }
            }));
        }
    }

    fn remove_observation_runtime(&self) {
        self.data_mut(|data| data.remove::<ObservationRuntime>(egui::Id::new(RUNTIME_ID)));
        CURRENT_CONTEXT.with(|slot| {
            if slot.borrow().as_ref().is_some_and(|context| context == self) {
                *slot.borrow_mut() = None;
            }
        });
    }

    fn observe_window(&self, public_title: &str) {
        let Some(runtime) = runtime(self) else {
            return;
        };
        let Some(content_rect) = protocol_rect(self.screen_rect()) else {
            return;
        };
        let Ok(title) = Content::public(public_title) else {
            return;
        };
        let Ok(scale) = Fixed6::checked_from_f32(self.pixels_per_point()) else {
            return;
        };
        let _ = runtime.record(
            self.cumulative_pass_nr(),
            default_window_key(),
            ObservationBody::Window(WindowObservation {
                title,
                content_rect,
                viewport_rect: content_rect,
                scale,
            }),
        );
    }

    fn finish_observation(&self) {
        if let Some(runtime) = runtime(self) {
            let _ = runtime.finish(self.cumulative_pass_nr(), default_window_key());
        }
    }
}

/// Run the ordinary egui frame and observe its final text shapes without
/// changing input, focus, rendering output or product state. This observes
/// render-output geometry, not pixel visibility, logical receipts or callbacks.
/// The default-window SDK contract is single-viewport; callers must keep
/// separate runtimes for independent native windows.
pub fn run_observed(context:&egui::Context,input:egui::RawInput,run_ui:impl FnMut(&egui::Context))->egui::FullOutput{
    struct Restore(egui::Context,Option<bool>);
    impl Drop for Restore{fn drop(&mut self){self.0.data_mut(|data|{let id=egui::Id::new(DEFER_COMPLETION_ID);match self.1{Some(previous)=>data.insert_temp(id,previous),None=>{data.remove::<bool>(id);}}});}}
    let previous=context.data_mut(|data|{let id=egui::Id::new(DEFER_COMPLETION_ID);let previous=data.get_temp::<bool>(id);data.insert_temp(id,true);previous});
    let restore=Restore(context.clone(),previous);
    let output=context.run(input,run_ui);
    if let Some(runtime)=runtime(context){
        let frame=context.cumulative_pass_nr().saturating_sub(1);
        if let Err(reason)=record_render_text(context,&runtime,frame,&output){
            let _=runtime.record(frame,default_window_key(),ObservationBody::RecordDropped(ui_observation_protocol::RecordDropped{reason,count:1}));
        }else{
            let _=runtime.record(frame,default_window_key(),ObservationBody::StateRevision(ui_observation_protocol::StateRevision{target:ElementKey::parse("ui.render.text.scan-complete").expect("render key"),revision:frame,cause_action_sequence:None}));
        }
        let _=runtime.record(frame,default_window_key(),ObservationBody::StateRevision(ui_observation_protocol::StateRevision{target:ElementKey::parse(ui_observation_protocol::FRAME_COMPLETE_KEY).expect("frame key"),revision:frame,cause_action_sequence:None}));
    }
    drop(restore);
    output
}

fn record_render_text(context:&egui::Context,runtime:&ObservationRuntime,frame:u64,output:&egui::FullOutput)->Result<(),ui_observation_protocol::DropReason>{
    use ui_observation_protocol::DropReason;
    let viewport=protocol_rect(context.screen_rect()).ok_or(DropReason::Serialization)?;
    let mut texts=0usize;
    fn observe(shape:&egui::epaint::Shape,clip:egui::Rect,viewport:Rect,runtime:&ObservationRuntime,frame:u64,texts:&mut usize)->Result<(),DropReason>{
        match shape{
            egui::epaint::Shape::Text(shape)=>{
                if !shape.opacity_factor.is_finite() || !shape.angle.is_finite(){return Err(DropReason::Serialization);}
                if shape.opacity_factor<=0.0 || shape.galley.mesh_bounds==egui::Rect::NOTHING{return Ok(());}
                let mut digest=ui_observation_protocol::ContentDigest::new();
                let mut painted_bytes=0usize;
                // The emitted record is always a digest. A large galley must
                // not allocate another plaintext copy or poison the stream
                // merely because it exceeds a small-label string limit.
                for row in &shape.galley.rows{
                    for glyph in &row.glyphs{
                        let mut utf8=[0u8;4];let bytes=glyph.chr.encode_utf8(&mut utf8).as_bytes();
                        digest.update(bytes).map_err(|_|DropReason::Serialization)?;
                        painted_bytes=painted_bytes.checked_add(bytes.len()).ok_or(DropReason::Oversized)?;
                    }
                    if row.ends_with_newline{
                        digest.update(b"\n").map_err(|_|DropReason::Serialization)?;
                        painted_bytes=painted_bytes.checked_add(1).ok_or(DropReason::Oversized)?;
                    }
                }
                let index=*texts;*texts=texts.checked_add(1).ok_or(DropReason::Oversized)?;
                let mesh=shape.galley.mesh_bounds;
                let mut bounds=egui::Rect::NOTHING;
                let (sin,cos)=shape.angle.sin_cos();
                for corner in [mesh.left_top(),mesh.right_top(),mesh.right_bottom(),mesh.left_bottom()]{
                    let rotated=shape.pos+egui::vec2(cos*corner.x-sin*corner.y,sin*corner.x+cos*corner.y);
                    if !rotated.x.is_finite()||!rotated.y.is_finite(){return Err(DropReason::Serialization);}
                    bounds.extend_with(rotated);
                }
                let layout=protocol_rect(bounds).ok_or(DropReason::Serialization)?;
                let clip=protocol_rect(clip).ok_or(DropReason::Serialization)?;
                let visible=Rect::intersect_all(&[layout,clip,viewport,viewport]).ok_or(DropReason::Serialization)?;
                let element=ElementObservation{key:ElementKey::parse(format!("ui.render.text.{index}")).map_err(|_|DropReason::Serialization)?,role:Role::Label,
                    accessible_name:Content::private_omitted(painted_bytes as u64),content:Some(digest.finish()),control:None,text_value:None,
                    widget_id:Content::stable_identifier(format!("render-text-index-{index}")).map_err(|_|DropReason::Serialization)?,parent:None,
                    layout_rect:layout,clip_rect:clip,viewport_rect:viewport,window_content_rect:viewport,visible_rect:visible,
                    visible_fraction:layout.visible_fraction(visible).map_err(|_|DropReason::Serialization)?,layer:i64::try_from(index).map_err(|_|DropReason::Oversized)?,
                    states:States{enabled:false,focused:false,hovered:false,pressed:false,clicked:false,selected:false,editable:false}};
                element.validate().map_err(|_|DropReason::Serialization)?;
                if runtime.record(frame,default_window_key(),ObservationBody::Element(element))==ui_observation_runtime::RecordStatus::Dropped{return Err(DropReason::QueueFull);}
            },_=>{},
        }Ok(())
    }
    // Traversal retains only iterators for the current ancestry. It neither
    // copies shapes nor recurses on the thread stack. Existing product text is
    // hashed as a stream, so its length does not allocate an observation copy.
    // Actual sink capacity and allocation failure govern retention, rather than
    // fixed shape, text-count, nesting or plaintext-byte quotas.
    for clipped in &output.shapes{
        let mut ancestry=vec![std::slice::from_ref(&clipped.shape).iter()];
        while let Some(parent)=ancestry.last_mut(){
            let Some(shape)=parent.next()else{ancestry.pop();continue;};
            if let egui::epaint::Shape::Vec(children)=shape{
                ancestry.try_reserve(1).map_err(|_|DropReason::QueueFull)?;
                ancestry.push(children.iter());
            }else{observe(shape,clipped.clip_rect,viewport,runtime,frame,&mut texts)?;}
        }
    }
    Ok(())
}

pub trait ResponseObservationExt: Sized {
    /// Records mechanically derived response facts and returns the exact same
    /// response value.
    #[must_use]
    fn observe(self, spec: ElementSpec) -> Self;

    /// Observe the actual post-widget editor value without exposing plaintext.
    /// This neither requests focus nor changes input or product state.
    #[must_use]
    fn observe_text_digest(self, spec: ElementSpec, actual_value: &str) -> Self;
}

impl ResponseObservationExt for egui::Response {
    fn observe_text_digest(self, spec: ElementSpec, actual_value: &str) -> Self {
        self.observe(spec.with_text_value(Content::digest_only(actual_value.as_bytes())))
    }

    fn observe(self, spec: ElementSpec) -> Self {
        if let Some((runtime, observation)) = element_observation(&self, spec) {
            let _ = runtime.record(
                self.ctx.cumulative_pass_nr(),
                default_window_key(),
                ObservationBody::Element(observation),
            );
        }
        self
    }
}

pub trait InnerResponseObservationExt: Sized {
    /// Observes the boundary response and returns the exact composite output.
    #[must_use]
    fn observe_region(self, spec: RegionSpec) -> Self;
}

impl<T> InnerResponseObservationExt for egui::InnerResponse<T> {
    fn observe_region(self, spec: RegionSpec) -> Self {
        record_response_region(&self.response, spec);
        self
    }
}

pub trait ScrollAreaObservationExt: Sized {
    /// Records standardized viewport/content/offset/scroll-range facts and
    /// returns the exact scroll output.
    #[must_use]
    fn observe_region(self, spec: RegionSpec) -> Self;
}

impl<T> ScrollAreaObservationExt for egui::scroll_area::ScrollAreaOutput<T> {
    fn observe_region(self, spec: RegionSpec) -> Self {
        CURRENT_CONTEXT.with(|slot| {
            if let Some(context) = slot.borrow().as_ref() {
                record_scroll_region(context, &self, spec);
            }
        });
        self
    }
}

fn runtime(context: &egui::Context) -> Option<ObservationRuntime> {
    context.data(|data| data.get_temp::<ObservationRuntime>(egui::Id::new(RUNTIME_ID)))
}

fn default_window_key() -> ElementKey {
    ElementKey::parse(DEFAULT_WINDOW_KEY).expect("constant key is valid")
}

fn protocol_rect(rect: egui::Rect) -> Option<Rect> {
    Rect::from_f32(rect.min.x, rect.min.y, rect.max.x, rect.max.y).ok()
}

fn element_observation(
    response: &egui::Response,
    spec: ElementSpec,
) -> Option<(ObservationRuntime, ElementObservation)> {
    let runtime = runtime(&response.ctx)?;
    let layout_rect = protocol_rect(response.rect)?;
    let clip_rect = protocol_rect(response.interact_rect)?;
    let viewport_rect = protocol_rect(response.ctx.screen_rect())?;
    let visible_rect = Rect::intersect_all(&[
        layout_rect,
        clip_rect,
        viewport_rect,
        viewport_rect,
    ])?;
    let visible_fraction = layout_rect.visible_fraction(visible_rect).ok()?;
    let widget_id = Content::stable_identifier(format!("{:?}", response.id)).ok()?;
    let observation = ElementObservation {
        key: spec.key,
        role: spec.role,
        accessible_name: spec.accessible_name,
        content: spec.content,
        control: spec.control,
        text_value: spec.text_value,
        widget_id,
        parent: spec.parent,
        layout_rect,
        clip_rect,
        viewport_rect,
        window_content_rect: viewport_rect,
        visible_rect,
        visible_fraction,
        layer: layer_number(response.layer_id.order),
        states: States {
            enabled: response.enabled(),
            focused: response.has_focus(),
            hovered: response.hovered(),
            pressed: response.is_pointer_button_down_on(),
            clicked: response.clicked(),
            selected: spec.selected,
            editable: spec.role == Role::TextInput,
        },
    };
    observation.validate().ok()?;
    Some((runtime, observation))
}

fn layer_number(order: egui::Order) -> i64 {
    match order {
        egui::Order::Background => 0,
        egui::Order::Middle => 1,
        egui::Order::Foreground => 2,
        egui::Order::Tooltip => 3,
        _ => 4,
    }
}

fn record_response_region(response: &egui::Response, spec: RegionSpec) {
    let Some(runtime) = runtime(&response.ctx) else {
        return;
    };
    let Some(layout_rect) = protocol_rect(response.rect) else {
        return;
    };
    let Some(clip_rect) = protocol_rect(response.interact_rect) else {
        return;
    };
    let Some(viewport_rect) = protocol_rect(response.ctx.screen_rect()) else {
        return;
    };
    let visible_rect = layout_rect.intersection(clip_rect).intersection(viewport_rect);
    let Ok(visible_fraction) = layout_rect.visible_fraction(visible_rect) else {
        return;
    };
    let size_x = Fixed6::from_millionths(layout_rect.max_x.millionths() - layout_rect.min_x.millionths());
    let size_y = Fixed6::from_millionths(layout_rect.max_y.millionths() - layout_rect.min_y.millionths());
    let region = RegionObservation {
        key: spec.key,
        role: spec.role,
        accessible_name: spec.accessible_name,
        parent: spec.parent,
        layout_rect,
        clip_rect,
        viewport_rect,
        window_content_rect: viewport_rect,
        visible_rect,
        visible_fraction,
        content_extent_x: size_x,
        content_extent_y: size_y,
        offset_x: Fixed6::ZERO,
        offset_y: Fixed6::ZERO,
        scroll_range_x: Fixed6::ZERO,
        scroll_range_y: Fixed6::ZERO,
        exposed_children: Vec::new(),
    };
    let _ = runtime.record(
        response.ctx.cumulative_pass_nr(),
        default_window_key(),
        ObservationBody::Region(region),
    );
}

fn record_scroll_region<T>(
    context: &egui::Context,
    output: &egui::scroll_area::ScrollAreaOutput<T>,
    spec: RegionSpec,
) {
    let Some(runtime) = runtime(context) else {
        return;
    };
    let Some(viewport_rect) = protocol_rect(output.inner_rect) else {
        return;
    };
    let Some(window_rect) = protocol_rect(context.screen_rect()) else {
        return;
    };
    let visible_rect = viewport_rect.intersection(window_rect);
    let Ok(visible_fraction) = viewport_rect.visible_fraction(visible_rect) else {
        return;
    };
    let Ok(content_x) = Fixed6::checked_from_f32(output.content_size.x) else {
        return;
    };
    let Ok(content_y) = Fixed6::checked_from_f32(output.content_size.y) else {
        return;
    };
    let Ok(offset_x) = Fixed6::checked_from_f32(output.state.offset.x) else {
        return;
    };
    let Ok(offset_y) = Fixed6::checked_from_f32(output.state.offset.y) else {
        return;
    };
    let viewport_width = output.inner_rect.width();
    let viewport_height = output.inner_rect.height();
    let Ok(scroll_x) = Fixed6::checked_from_f32((output.content_size.x - viewport_width).max(0.0)) else {
        return;
    };
    let Ok(scroll_y) = Fixed6::checked_from_f32((output.content_size.y - viewport_height).max(0.0)) else {
        return;
    };
    let region = RegionObservation {
        key: spec.key,
        role: spec.role,
        accessible_name: spec.accessible_name,
        parent: spec.parent,
        layout_rect: viewport_rect,
        clip_rect: viewport_rect,
        viewport_rect,
        window_content_rect: window_rect,
        visible_rect,
        visible_fraction,
        content_extent_x: content_x,
        content_extent_y: content_y,
        offset_x,
        offset_y,
        scroll_range_x: scroll_x,
        scroll_range_y: scroll_y,
        exposed_children: Vec::new(),
    };
    let _ = runtime.record(
        context.cumulative_pass_nr(),
        default_window_key(),
        ObservationBody::Region(region),
    );
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Arc, Mutex};
    use ui_observation_protocol::{Observation, Role};
    use ui_observation_runtime::{ObservationSink, RecordStatus};

    struct Capture(Mutex<Vec<Observation>>);
    impl ObservationSink for Capture {
        fn try_record(&self, observation: Observation) -> RecordStatus {
            self.0.lock().unwrap().push(observation);
            RecordStatus::Recorded
        }
    }

    #[test]
    fn final_render_text_is_digest_only_and_precedes_completed_frame_without_changing_output(){
        use ui_observation_runtime::{MemoryObserver,MemoryLimits};
        let capture=Arc::new(Capture(Mutex::new(Vec::new())));let context=egui::Context::default();
        context.install_observation_runtime(ObservationRuntime::with_sink("0123456789abcdef0123456789abcdef",capture.clone()));
        let output=run_observed(&context,egui::RawInput{screen_rect:Some(egui::Rect::from_min_size(egui::Pos2::ZERO,egui::vec2(320.,240.))),..Default::default()},|context|{
            context.observe_window("Render output");egui::CentralPanel::default().show(context,|ui|{ui.label("private painted sentinel");});
        });
        assert!(!output.shapes.is_empty());
        let records=capture.0.lock().unwrap();let mut observer=MemoryObserver::new("0123456789abcdef0123456789abcdef",MemoryLimits::default()).unwrap();
        for record in records.iter(){observer.push(record.clone()).unwrap();}
        let snapshot=observer.snapshot().unwrap();let texts=snapshot.latest.values().filter_map(|record|match &record.record{ObservationBody::Element(element)if element.key.as_str().starts_with("ui.render.text.")=>Some(element),_=>None}).collect::<Vec<_>>();
        assert!(texts.iter().any(|element|element.content==Some(Content::digest_only(b"private painted sentinel"))));
        assert!(texts.iter().all(|element|element.role==Role::Label&&element.control.is_none()&&!element.states.focused&&!element.states.editable));
        assert!(records.iter().all(|record|!String::from_utf8(ui_observation_protocol::canonical_json(record).unwrap()).unwrap().contains("private painted sentinel")));
        assert!(matches!(records.last().unwrap().record,ObservationBody::StateRevision(ref value)if value.target.as_str()==ui_observation_protocol::FRAME_COMPLETE_KEY));
    }
    #[test]
    fn render_scan_handles_empty_transparent_rotated_and_large_shape_collections() {
        let capture=Arc::new(Capture(Mutex::new(Vec::new())));
        let context=egui::Context::default();
        context.install_observation_runtime(ObservationRuntime::with_sink("0123456789abcdef0123456789abcdef",capture.clone()));
        let output=run_observed(&context,egui::RawInput{screen_rect:Some(egui::Rect::from_min_size(egui::Pos2::ZERO,egui::vec2(320.,240.))),..Default::default()},|context|{
            context.observe_window("Render cases");
            egui::CentralPanel::default().show(context,|ui|{
                ui.label("");ui.label("   ");
                let galley=ui.painter().layout_no_wrap("rotated".into(),egui::FontId::default(),egui::Color32::WHITE);
                ui.painter().add(egui::epaint::TextShape::new(egui::pos2(100.,100.),galley.clone(),egui::Color32::WHITE).with_angle(std::f32::consts::FRAC_PI_2));
                ui.painter().add(egui::epaint::TextShape::new(egui::pos2(100.,150.),galley,egui::Color32::WHITE).with_opacity_factor(0.));
            });
        });
        let records=capture.0.lock().unwrap();
        assert!(!records.iter().any(|r|matches!(r.record,ObservationBody::RecordDropped(_))));
        let rotated=records.iter().filter_map(|r|match &r.record{ObservationBody::Element(e)if e.content==Some(Content::digest_only(b"rotated"))=>Some(e),_=>None}).collect::<Vec<_>>();
        assert_eq!(rotated.len(),1,"transparent output must not be reported");
        let shape=output.shapes.iter().find_map(|s|match &s.shape{egui::epaint::Shape::Text(t)if t.angle!=0.=>Some(t),_=>None}).unwrap();
        let bounds=shape.galley.mesh_bounds;
        assert!((rotated[0].layout_rect.min_x.millionths() as f64/1_000_000.-(100.-bounds.max.y) as f64).abs()<0.001);
        assert!((rotated[0].layout_rect.max_y.millionths() as f64/1_000_000.-(100.+bounds.max.x) as f64).abs()<0.001);
        drop(records);
        let original=output.shapes.clone();
        let runtime=super::runtime(&context).unwrap();
        record_render_text(&context,&runtime,1,&output).unwrap();
        assert_eq!(original,output.shapes,"observation must preserve the complete render output");
        let mut oversized=output.clone();
        oversized.shapes=vec![egui::epaint::ClippedShape{clip_rect:egui::Rect::EVERYTHING,shape:egui::epaint::Shape::Noop};16385];
        record_render_text(&context,&runtime,2,&oversized).unwrap();
        let text=original.iter().find(|s|matches!(&s.shape,egui::epaint::Shape::Text(t)if t.angle!=0.)).unwrap().clone();
        oversized.shapes=vec![text.clone();600];
        capture.0.lock().unwrap().clear();
        record_render_text(&context,&runtime,3,&oversized).unwrap();
        assert_eq!(capture.0.lock().unwrap().len(),600,"text count must not silently truncate at 512");
        let mut nested=text.shape;
        for _ in 0..128{nested=egui::epaint::Shape::Vec(vec![nested]);}
        oversized.shapes=vec![egui::epaint::ClippedShape{clip_rect:text.clip_rect,shape:nested}];
        capture.0.lock().unwrap().clear();
        record_render_text(&context,&runtime,4,&oversized).unwrap();
        assert_eq!(capture.0.lock().unwrap().len(),1,"deep ancestry must retain its leaf without recursive scanning");
    }
    #[test]
    fn hundred_kib_render_text_keeps_exact_digest_without_a_plaintext_record() {
        let context=egui::Context::default();
        let capture=Arc::new(Capture(Mutex::new(Vec::new())));
        let runtime=ObservationRuntime::with_sink("0123456789abcdef0123456789abcdef",capture.clone());
        context.install_observation_runtime(runtime);
        let text=format!(" \né{}\n\t ","x".repeat(102400-7));
        assert_eq!(text.len(),102400);
        let output=run_observed(&context,egui::RawInput::default(),|context|{
            egui::CentralPanel::default().show(context,|ui|{
                let galley=ui.painter().layout_no_wrap(text.clone(),egui::FontId::default(),egui::Color32::WHITE);
                ui.painter().add(egui::epaint::TextShape::new(egui::pos2(0.,0.),galley,egui::Color32::WHITE));
            });
        });
        assert!(!output.shapes.is_empty());
        let records=capture.0.lock().unwrap();
        assert!(!records.iter().any(|r|matches!(r.record,ObservationBody::RecordDropped(_))));
        assert!(records.iter().any(|r|matches!(&r.record,ObservationBody::Element(e)if e.content==Some(Content::digest_only(text.as_bytes())))));
        let encoded=ui_observation_protocol::canonical_json(&*records).unwrap();
        assert!(encoded.len()<16384);
        assert!(!encoded.windows(256).any(|chunk|chunk==&text.as_bytes()[8..264]));
        assert!(records.iter().any(|r|matches!(&r.record,ObservationBody::StateRevision(revision)if revision.target.as_str()=="ui.render.text.scan-complete")));
        drop(records);
        capture.0.lock().unwrap().clear();
        let runtime=super::runtime(&context).unwrap();
        let mut repeated=output.clone();
        repeated.shapes=output.shapes.iter().cycle().take(output.shapes.len()*12).cloned().collect();
        record_render_text(&context,&runtime,1,&repeated).unwrap();
        let records=capture.0.lock().unwrap();
        assert_eq!(records.iter().filter(|r|matches!(&r.record,ObservationBody::Element(e)if e.content==Some(Content::digest_only(text.as_bytes())))).count(),12,"more than 1 MiB of existing text must remain observable without copying it into observations");
        assert!(ui_observation_protocol::canonical_json(&*records).unwrap().len()<32768);
    }
    #[test]
    fn completed_idle_pass_exposes_current_selection_and_editor_contents() {
        use ui_observation_runtime::{MemoryObserver,MemoryLimits};
        let nonce="0123456789abcdef0123456789abcdef";
        let sink=Arc::new(Capture(Mutex::new(Vec::new())));
        let context=egui::Context::default();
        let runtime=ObservationRuntime::with_sink(nonce,sink.clone());
        context.install_observation_runtime(runtime.clone());
        context.install_observation_runtime(runtime);
        let mut memory=MemoryObserver::new(nonce,MemoryLimits::default()).unwrap();
        for (selected,text) in [(true,"first input"),(false,"replaced input"),(true,"")] {
            let frame=context.cumulative_pass_nr();
            let mut text=text.to_owned();
            let _=context.run(egui::RawInput {screen_rect:Some(egui::Rect::from_min_size(egui::Pos2::ZERO,egui::vec2(320.,240.))),..Default::default()},|context| {
                context.observe_window("SDK live stream");
                egui::CentralPanel::default().show(context,|ui| {
                    let _=ui.add(egui::Button::new("Conversation").selected(selected)).observe(ElementSpec::new(ElementKey::parse("fixture.conversation").unwrap(),Role::ListItem,Content::public("Conversation").unwrap()).with_selected(selected));
                    let _=ui.text_edit_singleline(&mut text).observe_text_digest(ElementSpec::new(ElementKey::parse("fixture.editor").unwrap(),Role::TextInput,Content::public("Editor").unwrap()),&text);
                });
            });
            let records=std::mem::take(&mut *sink.0.lock().unwrap());
            assert_eq!(records.iter().filter(|record|matches!(&record.record,ObservationBody::StateRevision(revision) if revision.target.as_str()==ui_observation_protocol::FRAME_COMPLETE_KEY)).count(),1);
            for record in records {memory.push(record).unwrap();}
            let snapshot=memory.snapshot().unwrap();
            assert_eq!(snapshot.completed_frame,Some(frame));
            let ObservationBody::Element(conversation)=&snapshot.latest["fixture.conversation"].record else {panic!("conversation")};
            let ObservationBody::Element(editor)=&snapshot.latest["fixture.editor"].record else {panic!("editor")};
            assert_eq!(conversation.states.selected,selected);
            assert_eq!(editor.text_value,Some(Content::digest_only(text.as_bytes())));
        }
    }

    #[test]
    fn ordinary_response_is_returned_and_observed() {
        let sink = Arc::new(Capture(Mutex::new(Vec::new())));
        let context = egui::Context::default();
        context.install_observation_runtime(ObservationRuntime::with_sink(
            "0123456789abcdef0123456789abcdef",
            sink.clone(),
        ));
        let mut returned_id = None;
        let input = egui::RawInput {
            screen_rect: Some(egui::Rect::from_min_size(
                egui::Pos2::ZERO,
                egui::vec2(320.0, 200.0),
            )),
            ..Default::default()
        };
        let _ = context.run(input, |context| {
            egui::CentralPanel::default().show(context, |ui| {
                let original = ui.button("Primary action");
                let original_id = original.id;
                let returned = original.observe(ElementSpec::action(
                    ElementKey::parse("fixture.primary").unwrap(),
                    Role::Button,
                    Content::public("Primary action").unwrap(),
                ).with_content(Content::stable_identifier("7cae37ee-a840-4f53-bab0-5773b20aae2f").unwrap()));
                assert_eq!(returned.id, original_id);
                returned_id = Some(returned.id);
            });
        });
        assert!(returned_id.is_some());
        let records = sink.0.lock().unwrap();
        assert_eq!(records.iter().filter(|r|matches!(r.record,ObservationBody::Element(_))).count(), 1);
        let ObservationBody::Element(element) = &records[0].record else {
            panic!("expected element")
        };
        assert_eq!(element.key.as_str(), "fixture.primary");
        assert_eq!(element.content, Some(Content::stable_identifier("7cae37ee-a840-4f53-bab0-5773b20aae2f").unwrap()));
        assert_eq!(element.visible_fraction, ui_observation_protocol::Fraction::ONE);
    }

    #[test]
    fn unfocused_editor_reports_value_separately_from_reveal_declarations() {
        use ui_observation_protocol::{ControlMetadata, InteractionKind, RevealSpec};
        let sink = Arc::new(Capture(Mutex::new(Vec::new())));
        let context = egui::Context::default();
        context.install_observation_runtime(ObservationRuntime::with_sink(
            "0123456789abcdef0123456789abcdef", sink.clone()));
        let mut value = "private editor bytes".to_owned();
        let _ = context.run(egui::RawInput::default(), |context| {
            egui::CentralPanel::default().show(context, |ui| {
                let response = ui.text_edit_singleline(&mut value);
                let id = response.id;
                let response = response.observe_text_digest(ElementSpec::new(
                    ElementKey::parse("fixture.editor").unwrap(), Role::TextInput,
                    Content::public("Editor").unwrap()), &value);
                assert_eq!(id, response.id);
                assert!(!response.has_focus());
                let _ = ui.button("Open").observe(ElementSpec::action(
                    ElementKey::parse("fixture.open").unwrap(), Role::Button,
                    Content::public("Open").unwrap()).with_control(ControlMetadata {
                        interactions: vec![InteractionKind::Activate], scope: None,
                        reveals: vec![RevealSpec { on: InteractionKind::Activate,
                            target: ElementKey::parse("fixture.dialog").unwrap(), role: Role::Region }],
                    }));
            });
        });
        let records = sink.0.lock().unwrap();
        let ObservationBody::Element(editor) = &records[0].record else { panic!("editor") };
        assert_eq!(editor.text_value, Some(Content::digest_only(value.as_bytes())));
        assert!(!editor.states.focused);
        assert!(!String::from_utf8(ui_observation_protocol::canonical_json(&*records).unwrap()).unwrap().contains(&value));
        assert!(records.iter().all(|r| !matches!(&r.record,
            ObservationBody::Element(e) if e.key.as_str() == "fixture.dialog")));
    }

    #[test]
    fn scroll_output_reports_real_extents() {
        let sink = Arc::new(Capture(Mutex::new(Vec::new())));
        let context = egui::Context::default();
        context.install_observation_runtime(ObservationRuntime::with_sink(
            "0123456789abcdef0123456789abcdef",
            sink.clone(),
        ));
        let input = egui::RawInput {
            screen_rect: Some(egui::Rect::from_min_size(
                egui::Pos2::ZERO,
                egui::vec2(200.0, 100.0),
            )),
            ..Default::default()
        };
        let _ = context.run(input, |context| {
            egui::CentralPanel::default().show(context, |ui| {
                let _ = egui::ScrollArea::horizontal()
                    .show(ui, |ui| ui.allocate_space(egui::vec2(600.0, 20.0)))
                    .observe_region(RegionSpec::new("fixture.scroll", Role::ScrollView).unwrap());
            });
        });
        let records = sink.0.lock().unwrap();
        let ObservationBody::Region(region) = &records[0].record else {
            panic!("expected region")
        };
        assert!(region.content_extent_x > region.scroll_range_x);
        assert!(region.scroll_range_x > Fixed6::ZERO);
    }
}
