//! The editor's interaction state machine. Exactly one interaction is ever
//! active — the states that used to be four independent `Option` fields
//! (drawing drag, region drag, item drag, text edit) are one enum, so
//! "two drags at once" and friends are unrepresentable.
//!
//! States that mutate the document ([`EditorState::RegionDraw`],
//! [`EditorState::RegionAdjust`], [`EditorState::ItemsDrag`]) run inside an
//! open document transaction: entered with `doc.begin()`, left with
//! `doc.commit()` (pointer up) or `doc.rollback()` (Esc).

use eframe::egui::{Pos2, Rect};

use crate::annotate::{Annotation, Style};
use crate::document::AnnotationId;
use crate::editor::geometry::ResizeEdges;

/// What a region-box drag does.
#[derive(Clone, Copy, Debug)]
pub enum RegionMode {
    /// Translate the whole box; `grab` is where the pointer picked it up.
    Move { grab: Pos2 },
    /// Drag a handle; edges not in `edges` keep their start position.
    Resize { edges: ResizeEdges },
}

/// What an item-manipulation drag does to the grabbed annotation(s).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum ItemDragKind {
    Move,
    Resize { edges: ResizeEdges },
    /// Corner drag on markers: scales the font size, never stretches.
    ScaleUniform,
    Rotate,
    /// Line/arrow endpoint (`false` = start, `true` = end).
    Endpoint { second: bool },
}

/// Inline text editing session.
pub struct TextEditState {
    /// `Some` while re-editing an existing text annotation (hidden meanwhile).
    pub target: Option<AnnotationId>,
    pub pos: Pos2,
    pub buffer: String,
    pub style: Style,
    pub just_created: bool,
    /// The text's image-space rect the last time the overlay kept it inside
    /// the export bounds, so that work can be done when the text changes
    /// rather than every frame. `None` until the first measurement: merely
    /// re-opening text that already overhangs must not move it (that would
    /// record an undo step for looking at it) — editing is what fixes it.
    pub last_rect: Option<Rect>,
}

pub enum EditorState {
    Idle,
    /// Space+drag view pan (middle-drag pans without entering a state — it
    /// cannot conflict with a primary-button interaction).
    Panning,
    /// A drawing tool's rubber-band; the annotation is created on release.
    DrawingShape { start: Pos2, current: Pos2, points: Vec<Pos2> },
    /// Rubber-banding a new capture region (transactional).
    RegionDraw { anchor: Pos2 },
    /// Moving/resizing the capture region (transactional).
    RegionAdjust { start_rect: Rect, mode: RegionMode },
    /// Dragging the selected annotation(s); `items` holds pre-drag clones so
    /// every frame rebuilds from the start state (transactional).
    ItemsDrag {
        items: Vec<(AnnotationId, Annotation)>,
        kind: ItemDragKind,
        /// Bounding box of the dragged items at drag start, and its center
        /// (the rotate/scale pivot).
        bbox0: Rect,
        center: Pos2,
        grab: Pos2,
    },
    /// Shift+drag multi-select band (Select tool).
    RubberBand { anchor: Pos2, current: Pos2, additive: bool },
    /// Inline text editor open (keyboard belongs to it).
    TextEditing(TextEditState),
    /// First Esc at the root of the ladder: a second Esc before `until`
    /// quits and discards. Expires on `Editor::tick`; any other action
    /// dismisses it back to Idle.
    ConfirmDiscard { until: f64 },
}

impl EditorState {
    pub fn is_idle(&self) -> bool {
        matches!(self, EditorState::Idle)
    }

    pub fn is_text_editing(&self) -> bool {
        matches!(self, EditorState::TextEditing(_))
    }

    pub fn is_confirm_discard(&self) -> bool {
        matches!(self, EditorState::ConfirmDiscard { .. })
    }

    /// A pointer-driven interaction is in flight.
    pub fn is_pointer_op(&self) -> bool {
        !matches!(
            self,
            EditorState::Idle | EditorState::TextEditing(_) | EditorState::ConfirmDiscard { .. }
        )
    }
}
