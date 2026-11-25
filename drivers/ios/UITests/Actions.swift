// Input and device-action route handlers: touch, swipeV2, inputText, eraseText,
// pressKey, pressButton, setOrientation.
// Ported from Maestro 2.12.1 (`Routes/Handlers/{TouchRouteHandler,
// SwipeRouteHandlerV2,InputTextRouteHandler,EraseTextHandler,PressKeyHandler,
// PressButtonHandler,SetOrientationHandler}.swift`, Apache-2.0,
// Copyright (c) 2022 mobile.dev inc., modified for Tapwing).

import Foundation
import UIKit
import XCTest

@MainActor
enum Actions {
    private static let defaultTapDuration: TimeInterval = 0.1
    private static let keyTypingFrequency: UInt64 = 30
    private static let maxCharactersToErase = 10_000

    static func touch(_ request: HttpRequest) async -> HttpResponse {
        guard let body = try? JSONDecoder().decode(TouchRequest.self, from: request.body) else {
            return AppError(type: .precondition, message: "Incorrect request body provided for touch").httpResponse
        }
        guard body.x.isFinite && body.y.isFinite else {
            return AppError(type: .precondition, message: "Touch coordinates must be finite").httpResponse
        }
        let duration = body.duration ?? defaultTapDuration
        guard duration.isFinite && duration >= 0 else {
            return AppError(type: .precondition, message: "Touch duration must be finite and non-negative").httpResponse
        }

        let (width, height) = ScreenSizeHelper.physicalScreenSize()
        let point = ScreenSizeHelper.orientationAwarePoint(
            width: width,
            height: height,
            point: CGPoint(x: body.x, y: body.y)
        )
        let orientation = ScreenSizeHelper.currentInterfaceOrientation()

        do {
            try await withCheckedThrowingContinuation { (continuation: CheckedContinuation<Void, Error>) in
                let once = ContinuationOnceGuard(continuation)
                TapwingSynthesizeTouch(point, duration, orientation) { error in
                    once.resume(error: error)
                }
            }
            return HttpResponse.ok()
        } catch {
            return AppError(message: "Error tapping point: \(error.localizedDescription)").httpResponse
        }
    }

    static func swipeV2(_ request: HttpRequest) async -> HttpResponse {
        guard let body = try? JSONDecoder().decode(SwipeV2Request.self, from: request.body) else {
            return AppError(type: .precondition, message: "Incorrect request body provided for swipeV2").httpResponse
        }
        guard body.startX.isFinite && body.startY.isFinite && body.endX.isFinite && body.endY.isFinite else {
            return AppError(type: .precondition, message: "Swipe coordinates must be finite").httpResponse
        }
        guard body.duration.isFinite && body.duration >= 0 else {
            return AppError(type: .precondition, message: "Swipe duration must be finite and non-negative").httpResponse
        }

        let foregroundInfo = RunningApp.getForegroundAppInfo()
        let (width, height) = ScreenSizeHelper.physicalScreenSize(foregroundInfo: foregroundInfo)
        let start = ScreenSizeHelper.orientationAwarePoint(
            width: width,
            height: height,
            point: CGPoint(x: body.startX, y: body.startY)
        )
        let end = ScreenSizeHelper.orientationAwarePoint(
            width: width,
            height: height,
            point: CGPoint(x: body.endX, y: body.endY)
        )
        let orientation = ScreenSizeHelper.currentInterfaceOrientation()
        let description = "Swipe (v2) from (\(body.startX), \(body.startY)) to (\(body.endX), \(body.endY)) with \(body.duration) duration"

        let application = foregroundInfo?.app
            ?? XCUIApplication(bundleIdentifier: RunningApp.springboardBundleId)
        let targetElement = application.children(matching: .any).firstMatch

        var nsError: NSError?
        let ok = TapwingDispatchSwipe(targetElement, description, start, end, body.duration, orientation, &nsError)
        if let nsError {
            return classifySwipeError(nsError).httpResponse
        }
        if !ok {
            return AppError(message: "Swipe v2 request failed without an underlying error").httpResponse
        }
        return HttpResponse.ok()
    }

    static func inputText(_ request: HttpRequest, ticket: RequestTicket) async -> HttpResponse {
        guard let body = try? JSONDecoder().decode(InputTextRequest.self, from: request.body) else {
            return AppError(type: .precondition, message: "Incorrect request body provided for inputText").httpResponse
        }
        do {
            await TextInputHelper.waitUntilKeyboardIsPresented()
            try await TextInputHelper.inputText(body.text) { ticket.shouldStop }
            return HttpResponse.ok()
        } catch let stopped as TypingStopped {
            return stopped.appError.httpResponse
        } catch {
            return AppError(message: "Error inputting text: \(error.localizedDescription)").httpResponse
        }
    }

    static func eraseText(_ request: HttpRequest, ticket: RequestTicket) async -> HttpResponse {
        guard let body = try? JSONDecoder().decode(EraseTextRequest.self, from: request.body) else {
            return AppError(type: .precondition, message: "Incorrect request body provided for eraseText").httpResponse
        }
        guard body.charactersToErase >= 0 && body.charactersToErase <= maxCharactersToErase else {
            return AppError(
                type: .precondition,
                message: "charactersToErase must be between 0 and \(maxCharactersToErase)"
            ).httpResponse
        }
        if body.charactersToErase == 0 {
            return HttpResponse.ok()
        }
        do {
            let deleteText = String(repeating: XCUIKeyboardKey.delete.rawValue, count: body.charactersToErase)
            try await TextInputHelper.inputText(deleteText) { ticket.shouldStop }
            return HttpResponse.ok()
        } catch let stopped as TypingStopped {
            return stopped.appError.httpResponse
        } catch {
            return AppError(message: "Failure erasing text: \(error.localizedDescription)").httpResponse
        }
    }

    static func pressKey(_ request: HttpRequest) async -> HttpResponse {
        guard let body = try? JSONDecoder().decode(PressKeyRequest.self, from: request.body) else {
            return AppError(type: .precondition, message: "Incorrect request body provided for pressKey").httpResponse
        }
        let keyString: String
        switch body.key {
        case .delete: keyString = XCUIKeyboardKey.delete.rawValue
        case .return: keyString = XCUIKeyboardKey.return.rawValue
        case .enter: keyString = XCUIKeyboardKey.enter.rawValue
        case .tab: keyString = XCUIKeyboardKey.tab.rawValue
        case .space: keyString = XCUIKeyboardKey.space.rawValue
        case .escape: keyString = XCUIKeyboardKey.escape.rawValue
        }
        do {
            try await TextInputHelper.synthesizeText(
                keyString,
                typingSpeed: keyTypingFrequency,
                orientation: ScreenSizeHelper.currentInterfaceOrientation()
            )
            return HttpResponse.ok()
        } catch {
            return AppError(message: "Press key handler failed: \(error.localizedDescription)").httpResponse
        }
    }

    static func pressButton(_ request: HttpRequest) -> HttpResponse {
        guard let body = try? JSONDecoder().decode(PressButtonRequest.self, from: request.body) else {
            return AppError(type: .precondition, message: "Incorrect request body provided for pressButton").httpResponse
        }
        switch body.button {
        case .home:
            XCUIDevice.shared.press(.home)
        case .lock:
            TapwingPressLockButton()
        }
        return HttpResponse.ok()
    }

    static func setOrientation(_ request: HttpRequest, onOrientationChanged: (() -> Void)? = nil) -> HttpResponse {
        guard let body = try? JSONDecoder().decode(SetOrientationRequest.self, from: request.body) else {
            return AppError(type: .precondition, message: "Incorrect request body provided for setOrientation").httpResponse
        }
        switch body.orientation {
        case .portrait:
            XCUIDevice.shared.orientation = .portrait
        case .landscapeLeft:
            XCUIDevice.shared.orientation = .landscapeLeft
        case .landscapeRight:
            XCUIDevice.shared.orientation = .landscapeRight
        case .upsideDown:
            XCUIDevice.shared.orientation = .portraitUpsideDown
        }
        ScreenSizeHelper.invalidateCache()
        onOrientationChanged?()
        return HttpResponse.ok()
    }
}
