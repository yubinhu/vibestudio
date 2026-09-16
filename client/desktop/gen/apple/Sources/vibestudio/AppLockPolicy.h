#pragma once

#include <cstdint>

// Platform-independent state transitions. UIKit supplies a continuous clock
// including device sleep and calls background only for a real background event.
// Authentication is still performed exclusively by Apple's LAContext.
class VibeStudioLockPolicy {
public:
    static constexpr double graceSeconds = 60.0;
    enum class AuthenticationResult { ignored, failed, pendingActive, unlocked };

    bool unlocked() const { return unlocked_; }
    bool active() const { return active_; }
    bool backgrounded() const { return backgrounded_; }
    bool authenticating() const { return authenticating_; }

    void resignActive() { active_ = false; }

    void enterBackground(double now) {
        active_ = false;
        if (!backgrounded_) backgroundedAt_ = now;
        backgrounded_ = true;
        ++authenticationGeneration_;
        authenticating_ = false;
        pendingAuthenticationSuccess_ = false;
    }

    // Reports a newly locked transition so the native caller can synchronously
    // gate remote access. It does not change foreground/visibility state.
    bool refresh(double now) {
        if (unlocked_ && backgrounded_ && now - backgroundedAt_ >= graceSeconds) {
            unlocked_ = false;
            return true;
        }
        return false;
    }

    bool enterForeground(double now) {
        bool locked = refresh(now);
        backgrounded_ = false;
        return locked;
    }

    void becomeActive() {
        // A caller must assess the deadline before making content visible.
        if (backgrounded_) return;
        active_ = true;
        if (pendingAuthenticationSuccess_) {
            pendingAuthenticationSuccess_ = false;
            unlocked_ = true;
        }
    }

    // Zero means a prompt is already underway or not currently permitted.
    uint64_t beginAuthentication() {
        if (!active_ || backgrounded_ || unlocked_ || authenticating_) return 0;
        authenticating_ = true;
        return ++authenticationGeneration_;
    }

    AuthenticationResult finishAuthentication(uint64_t generation, bool success) {
        if (!authenticating_ || generation != authenticationGeneration_ || backgrounded_) {
            return AuthenticationResult::ignored;
        }
        authenticating_ = false;
        if (!success) return AuthenticationResult::failed;
        if (!active_) {
            pendingAuthenticationSuccess_ = true;
            return AuthenticationResult::pendingActive;
        }
        unlocked_ = true;
        return AuthenticationResult::unlocked;
    }

private:
    bool unlocked_ = false;
    bool active_ = false;
    bool backgrounded_ = false;
    bool authenticating_ = false;
    bool pendingAuthenticationSuccess_ = false;
    double backgroundedAt_ = 0;
    uint64_t authenticationGeneration_ = 0;
};
