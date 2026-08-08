//! macOS on-device dictation for the terminal (Cmd+Shift+V), built directly on `Speech.framework`
//! (`SFSpeechRecognizer`) and `AVFAudio` (`AVAudioEngine`) rather than routing through the
//! system's own dictation UI, so recognized text can be streamed straight into the focused
//! session's pty as it's heard.
//!
//! `objc2-speech`/`objc2-avf-audio` are generated against a newer major version of `objc2` than
//! the rest of this crate's AppKit interop (`macos.rs`/`macos_input_view.rs` use `objc2 = "0.5"`;
//! these need `0.6` — see the `objc2-speech-runtime`/`objc2-speech-foundation` comment in
//! Cargo.toml). Cargo resolves both simultaneously without conflict since 0.x versions are
//! semver-independent, but their `Retained<T>`/`NSString`/`NSError` types are NOT
//! interchangeable — this module is careful to stay entirely within the newer ecosystem and only
//! ever hand plain, owned `String`/`bool` values across the `AppEvent` channel back to `App`,
//! same as every other native thread -> main thread path in this app.

use std::ptr::NonNull;

use objc2_avf_audio::{AVAudioEngine, AVAudioPCMBuffer, AVAudioTime};
use objc2_speech::{
    SFSpeechAudioBufferRecognitionRequest, SFSpeechRecognitionResult, SFSpeechRecognizer,
    SFSpeechRecognizerAuthorizationStatus,
};
use objc2_speech_foundation::NSError;
use objc2_speech_runtime::rc::Retained;
use winit::event_loop::EventLoopProxy;

use crate::AppEvent;

/// One in-progress dictation session: the audio engine that's tapping the mic, and the streaming
/// request it's feeding PCM buffers into. `stop` drops both of these (via dropping `self`)
/// immediately once it's told the engine to stop and told the request there's no more audio
/// coming — well before the recognizer is actually done processing what's already buffered. That's fine
/// *only* because `start` separately keeps its own clones of the recognizer and this same
/// request alive inside the result-handler block's closure for exactly as long as the block
/// itself lives (see its comment there) — the copies here exist for `stop` to act on, not to
/// keep anything alive on the recognizer's behalf.
pub struct VoiceSession {
    engine: Retained<AVAudioEngine>,
    request: Retained<SFSpeechAudioBufferRecognitionRequest>,
}

/// Starts listening on the default input device and streaming audio to speech recognition
/// (on-device when the recognizer supports it, so nothing said is sent anywhere by default).
/// Must be called on the main thread, same as every other AppKit-adjacent call in this app.
///
/// Three outcomes:
/// - `Ok(Some(session))`: already authorized, recording started — caller stores `session` and
///   is now listening.
/// - `Ok(None)`: authorization hadn't been decided yet, so this only kicked off the system
///   permission prompt (`SFSpeechRecognizer::requestAuthorization`) and returns immediately —
///   its result arrives later as `AppEvent::VoiceAuthResult`, and `App` re-calls `start` once
///   that's `true`, at which point `authorizationStatus()` reads `Authorized` and this proceeds
///   straight through to actually recording. Nothing is listening yet in this branch.
/// - `Err(message)`: recording could not start at all (denied/restricted, no working recognizer,
///   or the audio engine itself refused to start) — safe to show `message` directly to the user.
pub fn start(proxy: EventLoopProxy<AppEvent>) -> Result<Option<VoiceSession>, String> {
    match unsafe { SFSpeechRecognizer::authorizationStatus() } {
        SFSpeechRecognizerAuthorizationStatus::Authorized => {}
        SFSpeechRecognizerAuthorizationStatus::NotDetermined => {
            let auth_block = block2::RcBlock::new(
                move |status: SFSpeechRecognizerAuthorizationStatus| {
                    let authorized = status == SFSpeechRecognizerAuthorizationStatus::Authorized;
                    let _ = proxy.send_event(AppEvent::VoiceAuthResult(authorized));
                },
            );
            // `requestAuthorization` retains its own copy of the block (standard Cocoa
            // callback-parameter convention — it has to, since it calls back long after this
            // function has returned), so `auth_block` dropping at the end of this scope is fine.
            unsafe { SFSpeechRecognizer::requestAuthorization(&auth_block) };
            return Ok(None);
        }
        _ => {
            return Err(
                "Speech recognition access is denied — enable it in System Settings › Privacy \
                 & Security › Speech Recognition, then try again."
                    .into(),
            )
        }
    }

    let recognizer = unsafe { SFSpeechRecognizer::new() };
    if !unsafe { recognizer.isAvailable() } {
        return Err("Speech recognition isn't available right now — try again in a moment.".into());
    }

    let request = unsafe { SFSpeechAudioBufferRecognitionRequest::new() };
    unsafe { request.setShouldReportPartialResults(true) };
    // Keeps dictated terminal input (which may well include secrets, file paths, commands)
    // off the network whenever the current language's model supports it locally.
    if unsafe { recognizer.supportsOnDeviceRecognition() } {
        unsafe { request.setRequiresOnDeviceRecognition(true) };
    }

    let engine = unsafe { AVAudioEngine::new() };
    let input_node = unsafe { engine.inputNode() };
    let format = unsafe { input_node.outputFormatForBus(0) };

    let tap_request = request.clone();
    let tap_block = block2::RcBlock::new(move |buffer: NonNull<AVAudioPCMBuffer>, _when: NonNull<AVAudioTime>| {
        // CAUTION (per Apple's docs on this callback): may run on a thread other than main.
        // `appendAudioPCMBuffer` is exactly the call Apple's own SpeakToMe sample makes from
        // this same callback, so that's assumed safe here too.
        unsafe { tap_request.appendAudioPCMBuffer(buffer.as_ref()) };
    });
    unsafe {
        input_node.installTapOnBus_bufferSize_format_block(
            0,
            4096,
            Some(&format),
            block2::RcBlock::as_ptr(&tap_block).cast(),
        );
    }
    // As with `auth_block` above: `installTapOnBus:...:block:` copies/retains the block itself,
    // so it keeps working correctly after this drops.
    drop(tap_block);

    if let Err(err) = unsafe { engine.startAndReturnError() } {
        unsafe { input_node.removeTapOnBus(0) };
        return Err(format!("Couldn't start the microphone: {}", err.localizedDescription()));
    }

    let result_proxy = proxy.clone();
    // Clones (ARC retains, not deep copies) kept alive by the closure itself for as long as the
    // framework keeps *it* alive — i.e. for the task's whole lifetime, however long after `stop`
    // (which drops `VoiceSession`'s own `recognizer`/`request` right away) that turns out to be.
    // Apple's own sample code (SpeakToMe) keeps the recognizer retained for the full recognition
    // session rather than releasing it as soon as `recognitionTask(with:resultHandler:)`
    // returns, and nothing in the docs guarantees the task holds its own strong reference back
    // to either object, so this is the conservative choice.
    let keep_alive_recognizer = recognizer.clone();
    let keep_alive_request = request.clone();
    let result_block = block2::RcBlock::new(
        move |result: *mut SFSpeechRecognitionResult, error: *mut NSError| {
            let _keep_alive = (&keep_alive_recognizer, &keep_alive_request);
            if let Some(result) = unsafe { result.as_ref() } {
                let text = unsafe { result.bestTranscription().formattedString().to_string() };
                let is_final = unsafe { result.isFinal() };
                let _ = result_proxy.send_event(AppEvent::VoiceTranscript { text, is_final });
                if is_final {
                    let _ = result_proxy.send_event(AppEvent::VoiceEnded(None));
                }
                return;
            }
            if let Some(error) = unsafe { error.as_ref() } {
                // Code 1110 in Apple's own (undocumented, but stable and widely relied-on)
                // `kAFAssistantErrorDomain` is "No speech detected" — the completely ordinary
                // case of the key being pressed and released without saying anything (a quick
                // accidental tap, or just changing your mind), not an actual failure. Surfacing
                // this as a big red error banner every time would be needlessly alarming for
                // something that isn't a problem; treat it the same as a normal empty result.
                if error.code() == 1110 {
                    let _ = result_proxy.send_event(AppEvent::VoiceEnded(None));
                    return;
                }
                let message = error.localizedDescription().to_string();
                let _ = result_proxy.send_event(AppEvent::VoiceEnded(Some(message)));
            }
        },
    );
    // Same convention again: `recognitionTaskWithRequest:resultHandler:` retains its own copy
    // of `result_block` for as long as the task runs, so it's fine for ours to be dropped once
    // this function returns (`result_block` isn't stored in `VoiceSession` below).
    let _task = unsafe { recognizer.recognitionTaskWithRequest_resultHandler(&request, &result_block) };

    Ok(Some(VoiceSession { engine, request }))
}

/// Ends a dictation session: stops capturing new audio and tells the recognizer no more is
/// coming. The already-buffered audio keeps being processed after this returns — the final
/// transcript (and `AppEvent::VoiceEnded`) still arrives asynchronously via the result handler
/// installed in `start`, which keeps its own clones of the recognizer/request alive independent
/// of `session` (see that function's comment) — safe to drop `session`'s copies of them here
/// immediately rather than waiting for that.
pub fn stop(session: VoiceSession) {
    unsafe {
        session.engine.inputNode().removeTapOnBus(0);
        session.engine.stop();
        session.request.endAudio();
    }
}
