// A virtual audio input the harness can unplug and plug back, to stand in for a USB receiver (V11, V15): an
// aggregate device that carries BlackHole 2ch's audio under a name and id of its own. It only changes what the
// system lists: no default device or routing is touched. The device is found by its id every time, so a copy left by an
// earlier run is taken over and removed, and it is removed again when this program ends.
//   virtual_input /path/to/control      the control file's first word: `up` (listed) or `gone` (removed)
import CoreAudio
import Foundation

let controlPath = CommandLine.arguments.count > 1 ? CommandLine.arguments[1] : "/tmp/virtual_input.control"
let uid = "com.lecturelive.verify.input"
let name = "LL Verify Input"

/// The device with this program's id, or 0 when none is listed.
func lookup() -> AudioObjectID {
    var uidRef = uid as CFString
    var id = AudioObjectID(0)
    var address = AudioObjectPropertyAddress(mSelector: kAudioHardwarePropertyTranslateUIDToDevice, mScope: kAudioObjectPropertyScopeGlobal, mElement: kAudioObjectPropertyElementMain)
    var size = UInt32(MemoryLayout<AudioObjectID>.size)
    let status = withUnsafeMutablePointer(to: &uidRef) { qualifier in
        AudioObjectGetPropertyData(AudioObjectID(kAudioObjectSystemObject), &address, UInt32(MemoryLayout<CFString>.size), qualifier, &size, &id)
    }
    return status == 0 ? id : 0
}

func create() {
    if lookup() != 0 { return }
    var device = AudioObjectID(0)
    let desc: [String: Any] = [
        kAudioAggregateDeviceNameKey: name,
        kAudioAggregateDeviceUIDKey: uid,
        kAudioAggregateDeviceSubDeviceListKey: [[kAudioSubDeviceUIDKey: "BlackHole2ch_UID"]],
        kAudioAggregateDeviceMainSubDeviceKey: "BlackHole2ch_UID",
        kAudioAggregateDeviceIsPrivateKey: 0,
        kAudioAggregateDeviceIsStackedKey: 0,
    ]
    let status = AudioHardwareCreateAggregateDevice(desc as CFDictionary, &device)
    print(status == 0 ? "listed \(name)" : "could not create (status \(status))")
    fflush(stdout)
}

func remove() {
    let id = lookup()
    if id == 0 { return }
    let status = AudioHardwareDestroyAggregateDevice(id)
    print(status == 0 ? "removed \(name)" : "could not remove (status \(status))")
    fflush(stdout)
}

func wanted() -> String {
    let text = (try? String(contentsOfFile: controlPath, encoding: .utf8)) ?? "up"
    return text.split(whereSeparator: { $0 == " " || $0 == "\n" }).first.map(String.init) ?? "up"
}

signal(SIGTERM) { _ in remove(); exit(0) }
signal(SIGINT) { _ in remove(); exit(0) }
remove() // a copy an earlier run left behind
create()
while true {
    let w = wanted()
    if w == "gone" { remove() } else { create() }
    Thread.sleep(forTimeInterval: 0.2)
}
