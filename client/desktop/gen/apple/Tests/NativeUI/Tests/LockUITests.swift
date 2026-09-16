import XCTest
import Vision
import Darwin

#if !targetEnvironment(simulator)
#error("This harness only supports the iPhone Simulator.")
#endif

// These notifications go to this simulator's BiometricKit service from its
// own test process. They are never linked into or enabled by the product app.
@_silgen_name("notify_post") private func qaNotifyPost(_ name: UnsafePointer<CChar>) -> UInt32
@_silgen_name("notify_register_check") private func qaNotifyRegister(_ name: UnsafePointer<CChar>, _ token: UnsafeMutablePointer<Int32>) -> UInt32
@_silgen_name("notify_set_state") private func qaNotifySetState(_ token: Int32, _ state: UInt64) -> UInt32
@_silgen_name("notify_cancel") private func qaNotifyCancel(_ token: Int32) -> UInt32

final class LockUITests: XCTestCase {
    private let app = XCUIApplication(bundleIdentifier: "one.vibestudio.app")
    private let springboard = XCUIApplication(bundleIdentifier: "com.apple.springboard")
    private var facePrompt: XCUIElement { springboard.staticTexts["Face ID"].firstMatch }
    private var connectionHeading: XCUIElement { app.staticTexts["Connect to a computer"].firstMatch }

    override func setUpWithError() throws {
        continueAfterFailure = false
        var token: Int32 = 0
        let name = "com.apple.BiometricKit.enrollmentChanged"
        XCTAssertEqual(name.withCString { qaNotifyRegister($0, &token) }, 0)
        defer { _ = qaNotifyCancel(token) }
        XCTAssertEqual(qaNotifySetState(token, 1), 0)
        XCTAssertEqual(name.withCString { qaNotifyPost($0) }, 0)
        app.launchArguments = ["-AppleLanguages", "(en)", "-AppleLocale", "en_US"]
        app.launchEnvironment["RUST_LOG"] = "warn,skill_server=debug,skill_core=debug,skill_client=debug"
    }

    private func capture(_ name: String, screenshot: XCUIScreenshot? = nil) {
        print("QA_STATE \(name)\nAPP:\n\(app.debugDescription)\nSPRINGBOARD:\n\(springboard.debugDescription)")
        let attachment = XCTAttachment(screenshot: screenshot ?? XCUIScreen.main.screenshot())
        attachment.name = name
        attachment.lifetime = .keepAlways
        add(attachment)
    }

    private func waitForFaceID() {
        // An erased simulator needs the app-specific Face ID consent once.
        // Handle only that named permission, never an unrelated system alert.
        let deadline = Date().addingTimeInterval(20)
        repeat {
            for owner in [app, springboard] {
                let alert = owner.alerts.firstMatch
                if alert.exists && alert.staticTexts.allElementsBoundByIndex.contains(where: { $0.label.contains("Face ID") }) {
                    for title in ["Allow", "OK"] {
                        let allow = alert.buttons[title]
                        if allow.exists { allow.tap(); break }
                    }
                }
            }
            if facePrompt.exists { return }
            Thread.sleep(forTimeInterval: 0.25)
        } while Date() < deadline
        capture("missing-face-id-prompt")
        XCTFail("The native Face ID prompt did not appear")
    }

    private func postBiometric(_ suffix: String) {
        let notification = "com.apple.BiometricKit_Sim.fingerTouch." + suffix
        XCTAssertEqual(notification.withCString { qaNotifyPost($0) }, 0)
    }

    private func authenticate() {
        waitForFaceID()
        postBiometric("match")
        XCTAssertTrue(connectionHeading.waitForExistence(timeout: 30), "The actual connection screen must load after authentication")
        let ready = NSPredicate { [weak self] _, _ in
            guard let self else { return false }
            return !self.facePrompt.exists && !self.app.buttons["Unlock"].exists && self.connectionHeading.isHittable
        }
        expectation(for: ready, evaluatedWith: nil)
        waitForExpectations(timeout: 15)
    }

    private func coldLaunch() {
        app.terminate()
        app.launch()
        waitForFaceID()
        XCTAssertFalse(connectionHeading.isHittable, "Workspace content must not be accessible before authentication")
    }

    func testNativeAuthenticationAndGrace() {
        coldLaunch()
        capture("01-cold-launch-face-id-and-private-cover")
        authenticate()
        capture("02-unlocked-connection-screen")

        XCUIDevice.shared.press(.home)
        Thread.sleep(forTimeInterval: 5)
        app.activate()
        XCTAssertTrue(connectionHeading.waitForExistence(timeout: 10))
        XCTAssertTrue(connectionHeading.isHittable)
        XCTAssertFalse(facePrompt.exists, "A short background stay must preserve the grace period")
        XCTAssertFalse(app.buttons["Unlock"].exists)
        capture("03-five-second-return-without-authentication")

        XCUIDevice.shared.press(.home)
        print("QA_WAIT beginning an actual 61-second background interval")
        Thread.sleep(forTimeInterval: 31)
        print("QA_WAIT 31 seconds elapsed")
        Thread.sleep(forTimeInterval: 30)
        app.activate()
        waitForFaceID()
        capture("04-one-minute-return-requires-face-id")
        XCTAssertFalse(connectionHeading.isHittable, "The retained workspace must be hidden from accessibility while covered")
        authenticate()
        capture("05-unlocked-after-expired-grace")

        coldLaunch()
        postBiometric("nomatch")
        let cancel = springboard.buttons["Cancel"].firstMatch
        XCTAssertTrue(cancel.waitForExistence(timeout: 10))
        capture("06-nonmatching-face")
        cancel.tap()
        XCTAssertTrue(app.buttons["Unlock"].waitForExistence(timeout: 10))
        XCTAssertTrue(app.buttons["Unlock"].isEnabled)
        XCTAssertFalse(connectionHeading.isHittable)
        capture("07-canceled-authentication-keeps-lock")
        app.buttons["Unlock"].tap()
        authenticate()
        capture("08-retry-after-cancel-unlocks")
    }

    func testAppSwitcherPrivacyCover() throws {
        coldLaunch()
        authenticate()
        let start = app.coordinate(withNormalizedOffset: CGVector(dx: 0.5, dy: 0.995))
        let end = app.coordinate(withNormalizedOffset: CGVector(dx: 0.5, dy: 0.45))
        start.press(forDuration: 0.1, thenDragTo: end, withVelocity: .slow, thenHoldForDuration: 1)
        let switcher = springboard.otherElements["AppSwitcherContentView"].firstMatch
        XCTAssertTrue(switcher.waitForExistence(timeout: 10), "The gesture must open the app switcher, not merely Home")
        let card = springboard.otherElements.matching(NSPredicate(format: "identifier BEGINSWITH %@", "card:one.vibestudio.app:")).firstMatch
        XCTAssertTrue(card.waitForExistence(timeout: 10))
        let preview = card.screenshot()
        capture("09-app-switcher-private-preview")
        capture("10-vibestudio-preview-card", screenshot: preview)
        let request = VNRecognizeTextRequest()
        request.recognitionLevel = .accurate
        request.recognitionLanguages = ["en-US"]
        try VNImageRequestHandler(cgImage: preview.image.cgImage!, options: [:]).perform([request])
        let lines = (request.results ?? []).compactMap { $0.topCandidates(1).first?.string.lowercased() }
        print("QA_PREVIEW_TEXT \(lines)")
        XCTAssertTrue(lines.contains(where: { $0.contains("vibestudio") }), "The cover title must be inside the app card")
        XCTAssertFalse(lines.contains(where: { $0.contains("connect") || $0.contains("computer") || $0.contains("ssh") }), "The connection screen must not leak into the preview")
        app.activate()
        XCTAssertTrue(connectionHeading.waitForExistence(timeout: 10))
        XCTAssertTrue(connectionHeading.isHittable, "Accessibility must be restored after leaving the app switcher")
        XCTAssertFalse(facePrompt.exists)
    }
}
