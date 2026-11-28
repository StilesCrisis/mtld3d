//! Presentation under the host's present clock (`mtld3d_shared::present_clock`).
//!
//! The host returns the game's `Present` on a clock of its own, so nothing
//! here paces the game: the display only has to show the newest finished
//! frame, and the game's rendering must never wait for the display. Three
//! things differ from the module's default arrangement.
//!
//! * Presents commit to a queue of their own. A present waiting for its
//!   drawable then holds nothing on the render queue, where Metal would
//!   otherwise run the next frame's buffers only after it.
//! * Every present reads a copy. The render queue no longer orders a present's
//!   read of the back buffer before the next frame's writes, so each
//!   present-bearing frame's own command buffer ends with a copy of its back
//!   buffer into a slot of a ring, and the present reads the slot. Two events
//!   order the queues on the GPU: the copy signals `copied` with the frame's
//!   sequence and the present waits for it; the present signals `shown` with
//!   its sequence, and the next copy into the same slot waits for that.
//! * The presenter takes a drawable first (`nextDrawable` blocks until one is
//!   free), then a frame. A frame the host named a time for is presented at
//!   it (`presentDrawable atTime:`), oldest first, an older one giving way only
//!   once a newer one is already due; a frame without one is the newest
//!   finished, older ones giving way. The queue between the game and the
//!   display is then never deeper than the drawable pool
//!   (`DRAWABLE_POOL_DEPTH`), so no frames pile up.
//! * What the display did with each frame the host named a time for is
//!   recorded for the host (`present_clock::note_shown`): when it was shown,
//!   or, for any frame dropped on the way (`drop_packet`), that it was not.
//!
//! Ring size: [`CLOCK_SLOTS`]. A copy that finds every slot read by a frame
//! still waiting takes the oldest waiting frame's slot: that frame would have
//! given way to a newer one anyway.

use std::sync::PoisonError;

use block2::RcBlock;
use mtld3d_shared::{mtl_handle::MTLCommandQueueKind, perf::NanosSetTimer};
use objc2::{rc::Retained, runtime::ProtocolObject};
use objc2_metal::{
    MTLBlitCommandEncoder, MTLCommandBuffer, MTLCommandBufferStatus, MTLCommandEncoder,
    MTLCommandQueue, MTLDevice, MTLDrawable, MTLEvent, MTLOrigin, MTLSize, MTLTexture,
};
use objc2_quartz_core::CAMetalLayer;

use super::{
    PresentPacket, PresentState, PresenterFlags, command, diagnostics, drop_packet, ensure_slot,
    resolve_when_presented,
};
use crate::{
    LOG_TARGET,
    metal::{
        handle::{IntoRetained, IntoRetainedLayer},
        macdrv::attachment,
        record::DeviceRecord,
    },
};

/// The log's rate: five decimals of a hertz, from a period in nanoseconds.
const HZ_FRACTION: u128 = 100_000;
const NS_PER_S: u128 = NS_PER_SEC as u128;

/// Nanoseconds in a second.
const NS_PER_SEC: u64 = 1_000_000_000;

/// Slots a present-bearing frame copies into, under the host's clock.
pub const CLOCK_SLOTS: usize = 4;
const _: () = assert!(
    CLOCK_SLOTS <= super::SNAPSHOT_SLOTS,
    "the slots are the snapshot ring's"
);

/// What a device presents with under the host's clock.
///
/// The queue of its presents and the two events that order them against the
/// render queue.
pub struct ClockPresent {
    queue: Retained<ProtocolObject<dyn MTLCommandQueue>>,
    /// Signalled by a frame's copy into its slot, with the frame's sequence.
    copied: Retained<ProtocolObject<dyn MTLEvent>>,
    /// Signalled by a present once it has read its slot, with its sequence.
    shown: Retained<ProtocolObject<dyn MTLEvent>>,
}

// SAFETY: Apple documents command queues as thread-safe (any thread creates
// command buffers on one) and events as objects command buffers on any queue
// encode waits and signals for. `commandBuffer` on the queue and encoding the
// events into a buffer are the only uses, from the submit and presenter
// threads; nothing here touches another surface of either.
unsafe impl Send for ClockPresent {}
// SAFETY: as above.
unsafe impl Sync for ClockPresent {}

impl ClockPresent {
    /// The host's clock as this device latches it, with its queue and events.
    ///
    /// `None` without a clock, or when Metal gives none of them (logged once).
    pub fn new(
        render_queue: mtld3d_shared::mtl_handle::MetalHandle<MTLCommandQueueKind>,
    ) -> Option<Self> {
        let clock = mtld3d_shared::present_clock::get()?;
        let device = render_queue.into_retained()?.device();
        let (Some(queue), Some(copied), Some(shown)) = (
            device.newCommandQueue(),
            device.newEvent(),
            device.newEvent(),
        ) else {
            mtld3d_shared::log_once_warn!(
                target: LOG_TARGET,
                "presenter: the host's present clock needs a queue and two events Metal did not \
                 give; this device presents as without it",
            );
            return None;
        };
        queue.setLabel(Some(&objc2_foundation::NSString::from_str(
            "mtld3d-present-queue",
        )));
        // The rate in hundred-thousandths of a hertz, in integers: den / num ns.
        let centi_milli_hz = u128::from(clock.den) * HZ_FRACTION * NS_PER_S / u128::from(clock.num);
        log::info!(
            target: LOG_TARGET,
            "presenter: the host's present clock ({}.{:05} Hz): presents on their own queue, \
             each reading a copy of its frame; the display shows the newest",
            centi_milli_hz / HZ_FRACTION,
            centi_milli_hz % HZ_FRACTION,
        );
        Some(Self {
            queue,
            copied,
            shown,
        })
    }
}

/// End a present-bearing frame with the copy its present reads.
///
/// Encodes into `cmd_buf`, the frame's own command buffer, after its work:
/// a wait for the slot's last present to have read it, the copy of the
/// packet's back buffer into the slot, and the signal its present waits for.
/// The packet reads the slot from then on. `false` when no copy could be
/// encoded (logged once); the presenter then drops the frame, since nothing
/// orders its read of the back buffer against the next frame.
pub fn copy_for_present(
    state: &PresentState,
    cmd_buf: &ProtocolObject<dyn MTLCommandBuffer>,
    packet: &mut PresentPacket,
) -> bool {
    let Some(clock) = state.clock.as_ref() else {
        return false;
    };
    let Some(source) = packet.source.into_retained() else {
        return false;
    };
    let mut inner = state.lock();
    let index = loop {
        if let Some(free) =
            (0..CLOCK_SLOTS).find(|&i| !inner.pending.iter().any(|p| p.slot == Some(i)))
        {
            break free;
        }
        // Every slot is read by a frame still waiting: the oldest gives way.
        let Some(oldest) = inner.pending.pop_front() else {
            return false;
        };
        mtld3d_shared::crumb!("clock:giveway", oldest.seq);
        drop_packet(&mut inner, oldest);
    };
    let device = cmd_buf.device();
    let Some(slot_texture) = ensure_slot(&mut inner, index, &source, &device) else {
        return false;
    };
    let last_shown = inner.slots[index].as_ref().map_or(0, |slot| slot.shown);
    if last_shown > 0 {
        cmd_buf.encodeWaitForEvent_value(&clock.shown, last_shown);
    }
    let Some(blit) = cmd_buf.blitCommandEncoder() else {
        mtld3d_shared::log_once_warn!(
            target: LOG_TARGET,
            "presenter: blitCommandEncoder() returned nil for a frame's copy; the frame is not presented",
        );
        return false;
    };
    blit.setLabel(Some(&objc2_foundation::NSString::from_str(
        "mtld3d-present-copy",
    )));
    let origin = MTLOrigin { x: 0, y: 0, z: 0 };
    let size = MTLSize {
        width: source.width(),
        height: source.height(),
        depth: 1,
    };
    // SAFETY: objc2 typed binding; both textures are live, same format and
    // extent (`ensure_slot`), and the region is their whole level 0.
    unsafe {
        blit.copyFromTexture_sourceSlice_sourceLevel_sourceOrigin_sourceSize_toTexture_destinationSlice_destinationLevel_destinationOrigin(
            &source, 0, 0, origin, size, &slot_texture, 0, 0, origin,
        );
    }
    blit.endEncoding();
    cmd_buf.encodeSignalEvent_value(&clock.copied, packet.seq);
    if let Some(slot) = inner.slots[index].as_mut() {
        slot.reader = packet.seq;
    }
    drop(inner);
    packet.slot = Some(index);
    mtld3d_shared::crumb!("clock:copy", packet.seq, index as u64);
    true
}

/// One presenter iteration under the host's clock.
///
/// A drawable, then the frame it shows: the oldest waiting one at the time the
/// host gave it, or, for a frame without one, the newest finished. `false`
/// once the state is stopped.
pub fn present_frame(record: &std::sync::Arc<DeviceRecord>, clock: &ClockPresent) -> bool {
    let state = record.present();
    let (layer, view) = {
        let inner = state.lock();
        let mut inner = state
            .presenter_cv
            .wait_while(inner, |inner| {
                inner.pending.is_empty() && !inner.flags.contains(PresenterFlags::STOP)
            })
            .unwrap_or_else(PoisonError::into_inner);
        if inner.flags.contains(PresenterFlags::STOP) {
            while let Some(packet) = inner.pending.pop_front() {
                drop_packet(&mut inner, packet);
            }
            state.submit_cv.notify_all();
            return false;
        }
        let newest = inner.pending.back().expect("the wait ended on a packet");
        let picked = (newest.layer, newest.view);
        drop(inner);
        picked
    };
    let attachment = attachment::find(view);
    // An occluded window shows nothing, and its compositor recycles no
    // drawables: every frame waiting is dropped, as `nextDrawable` would only
    // run out its timeout.
    if attachment.as_ref().is_some_and(|att| att.window_occluded()) && !state.present_occluded {
        drop_all(state, attachment.as_deref());
        return true;
    }
    let Some(layer): Option<Retained<CAMetalLayer>> = IntoRetainedLayer::into_retained(layer)
    else {
        drop_all(state, attachment.as_deref());
        return true;
    };
    super::super::layer::sync_drawable_size(&layer);
    let mut drawable_wait_ns: u64 = 0;
    let drawable = {
        let _wait = NanosSetTimer::start(&raw mut drawable_wait_ns);
        layer.nextDrawable()
    };
    state.note_stall(drawable.is_none());
    let Some(drawable) = drawable else {
        drop_all(state, attachment.as_deref());
        return true;
    };

    let mut inner = state.lock();
    // A frame the host gave a time is presented at it, in order: an older one
    // gives way only once a newer frame is already due, as it could no longer
    // be shown at its own time. Any other frame: the newest finished one,
    // older ones giving way.
    let now_ns = host_now_ns();
    while inner.pending.len() > 1 {
        let next_due = inner.pending.get(1).map_or(0, |next| next.present_at_ns);
        let scheduled = inner
            .pending
            .front()
            .is_some_and(|front| front.present_at_ns != 0);
        if scheduled && next_due > now_ns {
            break;
        }
        let older = inner.pending.pop_front().expect("more than one is waiting");
        if let Some(att) = attachment.as_deref() {
            att.take_present();
        }
        drop_packet(&mut inner, older);
    }
    let Some(front) = inner.pending.front() else {
        return true;
    };
    let seq = front.seq;
    let present_at_ns = front.present_at_ns;
    let number = attachment
        .as_deref()
        .map(attachment::Attachment::take_present);
    let Some(source) = front
        .slot
        .and_then(|index| inner.slots[index].as_ref())
        .and_then(|slot| slot.texture.into_retained())
    else {
        // No copy was made: nothing orders a read of the back buffer.
        super::pop_and_drop(&mut inner, state);
        return true;
    };
    let index = front.slot.expect("a slot was read above");
    let Some(cb) = diagnostics::command_buffer(&clock.queue) else {
        mtld3d_shared::log_once_warn!(
            target: LOG_TARGET,
            "presenter: commandBuffer() returned nil on the present queue; the frame is not presented",
        );
        super::pop_and_drop(&mut inner, state);
        return true;
    };
    cb.setLabel(Some(&objc2_foundation::NSString::from_str(&format!(
        "mtld3d-present-{seq:#x}"
    ))));
    cb.encodeWaitForEvent_value(&clock.copied, seq);
    command::encode_present(
        &cb,
        &drawable,
        &command::PresentEncode {
            record,
            attachment: attachment.as_ref(),
            source: &source,
            seq,
            drawable_wait_ns,
            host_clock: true,
            present_at_ns,
        },
    );
    cb.encodeSignalEvent_value(&clock.shown, seq);
    if let Some(slot) = inner.slots[index].as_mut() {
        slot.shown = seq;
    }
    resolve_when_presented(&drawable, None, attachment, false, number);
    if present_at_ns != 0 {
        note_when_shown(&drawable, present_at_ns);
    }
    super::super::upscale::retire_evicted(&cb, record.upscale());
    let present_ptr = state.present_retired_ptr();
    let owner = std::sync::Arc::clone(record);
    let handler = RcBlock::new(
        move |cb_ptr: core::ptr::NonNull<ProtocolObject<dyn MTLCommandBuffer>>| {
            // SAFETY: Metal invokes the block with the buffer it ended; the
            // pointer is valid for the handler's duration.
            let cb = unsafe { cb_ptr.as_ref() };
            if !command::ended(cb) {
                return;
            }
            owner
                .gpu_time()
                .record(mtld3d_shared::perf::CommandBufferRole::Present, cb);
            if cb.status() == MTLCommandBufferStatus::Error {
                let (code, desc) = command::command_buffer_error(cb.error().as_deref());
                mtld3d_shared::log_once_warn_by!(
                    target: LOG_TARGET,
                    key: code,
                    "presenter: present buffer for frame seq={seq} failed on the GPU (code \
                     {code}: {desc}); the drawable showed undefined memory",
                );
            }
            command::retire_finished(owner.pending(), present_ptr, 0, "present-retire");
        },
    );
    // SAFETY: objc2 typed binding; Metal retains the block on registration.
    unsafe { cb.addCompletedHandler(RcBlock::as_ptr(&handler)) };
    mtld3d_shared::crumb!("present:commit", seq);
    command::commit_registered(record.pending(), &cb, present_ptr, seq);
    let packet = inner.pending.pop_front();
    inner.committed_present_seq = inner.committed_present_seq.max(seq);
    inner.presented_seq = seq;
    inner.note_drawable_wait(drawable_wait_ns);
    drop(inner);
    state.submit_cv.notify_all();
    drop(packet);
    true
}

/// Record, for the host, when the display shows a frame it named a time for.
fn note_when_shown(
    drawable: &ProtocolObject<dyn objc2_quartz_core::CAMetalDrawable>,
    present_at_ns: u64,
) {
    let handler = RcBlock::new(
        move |drawable: core::ptr::NonNull<ProtocolObject<dyn objc2_metal::MTLDrawable>>| {
            // SAFETY: Metal invokes the block with the presented drawable;
            // the pointer is valid for the handler's duration.
            let presented_s = unsafe { drawable.as_ref() }.presentedTime();
            mtld3d_shared::present_clock::note_shown(present_at_ns, presented_s);
        },
    );
    // SAFETY: objc2 typed binding; Metal copies (retains) the block on registration.
    unsafe { drawable.addPresentedHandler(RcBlock::as_ptr(&handler)) };
}

/// The host's clock now, in the nanoseconds a `present_at_ns` counts (`mach_absolute_time`'s base).
fn host_now_ns() -> u64 {
    let mut ts = libc::timespec {
        tv_sec: 0,
        tv_nsec: 0,
    };
    // SAFETY: writes the clock's time into the local timespec.
    unsafe { libc::clock_gettime(libc::CLOCK_UPTIME_RAW, &raw mut ts) };
    u64::try_from(ts.tv_sec).unwrap_or(0) * NS_PER_SEC + u64::try_from(ts.tv_nsec).unwrap_or(0)
}

/// Drop every frame waiting: its window shows nothing, or no drawable came.
fn drop_all(state: &PresentState, attachment: Option<&attachment::Attachment>) {
    {
        let mut inner = state.lock();
        while let Some(packet) = inner.pending.pop_front() {
            if let Some(att) = attachment {
                att.take_present();
            }
            drop_packet(&mut inner, packet);
        }
    }
    state.submit_cv.notify_all();
}
