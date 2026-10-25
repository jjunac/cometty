//! One submission/texture-release boundary for every returned frame outcome.
//!
//! Queue writes allocate staging memory before submission. Skipping a surface
//! frame (or returning a preparation error) must not leave those writes queued
//! until some future successful presentation. This small boundary is shared by
//! the real renderer and headless fault tests; it does not wait for the GPU.

pub(super) trait Backend {
    type CommandBuffer;

    fn is_device_lost(&self) -> bool;
    fn submit(&mut self, commands: impl IntoIterator<Item = Self::CommandBuffer>);
    fn update_texture(&mut self, id: egui::TextureId, delta: &egui::epaint::ImageDelta);
    fn free_texture(&mut self, id: egui::TextureId);
}

#[derive(Default)]
pub(super) struct Frame {
    pending_uploads: bool,
    textures: egui::TexturesDelta,
}

impl Frame {
    /// Mark before calling upload-capable dependencies: even an error result
    /// from glyphon's prepare can leave partial atlas uploads behind.
    pub(super) fn upload<B: Backend, T>(
        &mut self,
        backend: &mut B,
        upload: impl FnOnce(&mut B) -> T,
    ) -> Option<T> {
        if backend.is_device_lost() {
            return None;
        }
        self.pending_uploads = true;
        Some(upload(backend))
    }

    /// Consume this pass's deltas even when presentation is unavailable. Never
    /// retain an ever-growing CPU list of partial texture updates across frames.
    pub(super) fn update_textures<B: Backend>(
        &mut self,
        backend: &mut B,
        textures: egui::TexturesDelta,
    ) -> bool {
        debug_assert!(self.textures.is_empty());
        self.textures = textures;
        if backend.is_device_lost() {
            // Device reconstruction is not implemented. These GPU updates can
            // no longer be applied, but must be explicitly consumed for egui.
            self.textures.set.clear();
            return false;
        }
        for (id, deltas) in std::mem::take(&mut self.textures.set) {
            for delta in deltas {
                if self
                    .upload(backend, |backend| backend.update_texture(id, &delta))
                    .is_none()
                {
                    return false;
                }
            }
        }
        !backend.is_device_lost()
    }

    /// A normal submission also flushes uploads; cleanup must not submit twice.
    /// Confirmed device loss is not a recoverable surface-acquisition failure.
    pub(super) fn submit<B: Backend>(
        &mut self,
        backend: &mut B,
        commands: impl IntoIterator<Item = B::CommandBuffer>,
    ) -> bool {
        if backend.is_device_lost() {
            return false;
        }
        backend.submit(commands);
        self.pending_uploads = false;
        !backend.is_device_lost()
    }
}

/// Keep all `return`/`?` exits inside `draw`, so cleanup runs for both skipped
/// frames and errors. Texture destruction must follow submission: egui's
/// `free_texture` calls `Texture::destroy`, not just a reference-counted drop.
pub(super) fn run<B: Backend, T>(backend: &mut B, draw: impl FnOnce(&mut B, &mut Frame) -> T) -> T {
    let mut frame = Frame::default();
    let result = draw(backend, &mut frame);
    if frame.pending_uploads && !backend.is_device_lost() {
        crate::perf::add(crate::perf::Metric::UploadFlush, 1);
        frame.submit(backend, []);
    }
    for id in frame.textures.free.drain() {
        backend.free_texture(id);
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Debug, PartialEq, Eq)]
    enum Event {
        Upload,
        Texture(egui::TextureId, Option<[usize; 2]>),
        Submit(Vec<u8>),
        Free(egui::TextureId),
    }

    #[derive(Default)]
    struct FakeBackend {
        lost: bool,
        lose_on_submit: bool,
        pending_bytes: usize,
        peak_pending_bytes: usize,
        events: Vec<Event>,
    }

    impl FakeBackend {
        fn write(&mut self, bytes: usize) {
            self.pending_bytes += bytes;
            self.peak_pending_bytes = self.peak_pending_bytes.max(self.pending_bytes);
            self.events.push(Event::Upload);
        }
    }

    impl Backend for FakeBackend {
        type CommandBuffer = u8;

        fn is_device_lost(&self) -> bool {
            self.lost
        }

        fn submit(&mut self, commands: impl IntoIterator<Item = u8>) {
            assert!(!self.lost);
            self.events
                .push(Event::Submit(commands.into_iter().collect()));
            self.pending_bytes = 0;
            self.lost = self.lose_on_submit;
        }

        fn update_texture(&mut self, id: egui::TextureId, delta: &egui::epaint::ImageDelta) {
            self.write(delta.image.width() * delta.image.height() * 4);
            self.events.push(Event::Texture(id, delta.pos));
        }

        fn free_texture(&mut self, id: egui::TextureId) {
            assert!(
                self.lost || self.pending_bytes == 0,
                "destroy before submit"
            );
            self.events.push(Event::Free(id));
        }
    }

    fn texture_delta(free: bool) -> egui::TexturesDelta {
        let mut textures = egui::TexturesDelta::default();
        textures.push(
            egui::TextureId::Managed(7),
            egui::epaint::ImageDelta::full(
                egui::ColorImage::filled([2, 2], egui::Color32::WHITE),
                egui::TextureOptions::LINEAR,
            ),
        );
        if free {
            textures.free(egui::TextureId::Managed(7));
        }
        textures
    }

    #[test]
    fn repeated_skipped_frames_flush_updates_before_freeing_textures() {
        let mut backend = FakeBackend::default();
        // The surface failure is injected by returning before GPU frame work,
        // just like all acquisition-failure branches in render_frame.
        for _ in 0..10_000 {
            let actions = run(&mut backend, |backend, frame| {
                assert!(frame.update_textures(backend, texture_delta(true)));
                (Some(3), std::time::Duration::from_millis(16))
            });
            assert_eq!(actions, (Some(3), std::time::Duration::from_millis(16)));
            assert_eq!(backend.pending_bytes, 0);
            assert_eq!(
                backend.events,
                [
                    Event::Upload,
                    Event::Texture(egui::TextureId::Managed(7), None),
                    Event::Submit(vec![]),
                    Event::Free(egui::TextureId::Managed(7)),
                ]
            );
            backend.events.clear();
        }
        assert_eq!(backend.peak_pending_bytes, 16);
    }

    #[test]
    fn errors_after_partial_uploads_share_the_same_cleanup() {
        for fail_prepare in [true, false] {
            let mut backend = FakeBackend::default();
            let result: Result<(), &str> = run(&mut backend, |backend, frame| {
                frame.update_textures(backend, texture_delta(true));
                frame.upload(backend, |backend| backend.write(1_440_000));
                // A dependency is allowed to queue writes before returning Err.
                frame
                    .upload(backend, |backend| {
                        backend.write(4096);
                        if fail_prepare { Err("prepare") } else { Ok(()) }
                    })
                    .unwrap()?;
                Err("encode")
            });
            assert_eq!(result, Err(if fail_prepare { "prepare" } else { "encode" }));
            assert_eq!(backend.pending_bytes, 0);
            assert_eq!(
                &backend.events[backend.events.len() - 2..],
                [
                    Event::Submit(vec![]),
                    Event::Free(egui::TextureId::Managed(7))
                ]
            );
        }
    }

    #[test]
    fn successful_frame_submits_once_including_callback_commands() {
        let mut backend = FakeBackend::default();
        run(&mut backend, |backend, frame| {
            frame.update_textures(backend, texture_delta(true));
            frame.upload(backend, |backend| backend.write(1024));
            assert!(frame.submit(backend, [1, 2, 3]));
        });
        assert_eq!(
            &backend.events[backend.events.len() - 2..],
            [
                Event::Submit(vec![1, 2, 3]),
                Event::Free(egui::TextureId::Managed(7))
            ]
        );
        assert_eq!(backend.events.len(), 5);
    }

    #[test]
    fn partial_texture_deltas_are_applied_in_order_not_just_the_last_patch() {
        let mut backend = FakeBackend::default();
        run(&mut backend, |backend, frame| {
            let mut textures = texture_delta(true);
            for pos in [[0, 0], [1, 0]] {
                textures.push(
                    egui::TextureId::Managed(7),
                    egui::epaint::ImageDelta::partial(
                        pos,
                        egui::ColorImage::filled([1, 1], egui::Color32::RED),
                        egui::TextureOptions::LINEAR,
                    ),
                );
            }
            assert!(frame.update_textures(backend, textures));
        });
        assert_eq!(
            backend.events,
            [
                Event::Upload,
                Event::Texture(egui::TextureId::Managed(7), None),
                Event::Upload,
                Event::Texture(egui::TextureId::Managed(7), Some([0, 0])),
                Event::Upload,
                Event::Texture(egui::TextureId::Managed(7), Some([1, 0])),
                Event::Submit(vec![]),
                Event::Free(egui::TextureId::Managed(7)),
            ]
        );
    }

    #[test]
    fn successful_frame_after_many_failures_has_no_stale_pending_uploads() {
        let mut backend = FakeBackend::default();
        for _ in 0..1000 {
            let result: Result<(), ()> = run(&mut backend, |backend, frame| {
                frame.upload(backend, |backend| backend.write(4096));
                Err(())
            });
            assert_eq!(result, Err(()));
            assert_eq!(backend.pending_bytes, 0);
            backend.events.clear();
        }
        let presented = run(&mut backend, |backend, frame| {
            frame.upload(backend, |backend| backend.write(8192));
            frame.submit(backend, [1])
        });
        assert!(presented);
        assert_eq!(backend.events, [Event::Upload, Event::Submit(vec![1])]);
        assert_eq!(backend.pending_bytes, 0);
        assert_eq!(backend.peak_pending_bytes, 8192);
    }

    #[test]
    fn skipped_frame_without_uploads_does_not_submit() {
        let mut backend = FakeBackend::default();
        run(&mut backend, |_, _| {});
        assert!(backend.events.is_empty());
        // Frees alone need no new submission: previous frames have already
        // submitted any work that could refer to these textures.
        run(&mut backend, |backend, frame| {
            let mut textures = egui::TexturesDelta::default();
            textures.free(egui::TextureId::Managed(7));
            frame.update_textures(backend, textures);
        });
        assert_eq!(backend.events, [Event::Free(egui::TextureId::Managed(7))]);
    }

    #[test]
    fn confirmed_device_loss_stops_uploads_and_submissions() {
        let mut backend = FakeBackend {
            lost: true,
            ..Default::default()
        };
        run(&mut backend, |backend, frame| {
            assert!(!frame.update_textures(backend, texture_delta(true)));
            assert_eq!(
                frame.upload(backend, |_| panic!("upload on lost device")),
                None
            );
            assert!(!frame.submit(backend, [1]));
        });
        assert_eq!(backend.events, [Event::Free(egui::TextureId::Managed(7))]);
    }

    #[test]
    fn device_loss_during_upload_does_not_try_to_flush_a_dead_queue() {
        let mut backend = FakeBackend::default();
        run(&mut backend, |backend, frame| {
            frame.update_textures(backend, texture_delta(true));
            backend.lost = true;
            assert_eq!(frame.upload(backend, |_| panic!("upload after loss")), None);
        });
        assert!(!backend.events.iter().any(|e| matches!(e, Event::Submit(_))));
        assert_eq!(
            backend.events.last(),
            Some(&Event::Free(egui::TextureId::Managed(7)))
        );
    }

    #[test]
    fn device_loss_reported_by_submission_does_not_present_or_submit_again() {
        let mut backend = FakeBackend {
            lose_on_submit: true,
            ..Default::default()
        };
        let submitted = run(&mut backend, |backend, frame| {
            frame.update_textures(backend, texture_delta(true));
            frame.submit(backend, [1])
        });
        assert!(!submitted);
        assert_eq!(
            &backend.events[backend.events.len() - 2..],
            [
                Event::Submit(vec![1]),
                Event::Free(egui::TextureId::Managed(7))
            ]
        );
        assert_eq!(backend.events.len(), 4);
    }
}
