// Device state and screen-capture route handlers: deviceInfo,
// isScreenStatic, runningApp, keyboard, screenshot.
// Ported from Maestro 2.12.1 (`Routes/Handlers/{StatusHandler,DeviceInfoHandler,
// ScreenDiffHandler,RunningAppRouteHandler,KeyboardRouteHandler,
// ScreenshotHandler}.swift`, Apache-2.0, Copyright (c) 2022 mobile.dev inc.,
// modified for Tapwing).

import CryptoKit
import Foundation
import UIKit
import XCTest

@MainActor
enum ScreenProbe {
    static func deviceInfo() -> HttpResponse {
        let (width, height, _) = ScreenSizeHelper.actualScreenSize()
        let scale = Double(UIScreen.main.scale)
        guard width.isFinite, height.isFinite, scale.isFinite,
              width >= 0, height >= 0, scale > 0 else {
            return AppError(
                message: "Invalid screen dimensions: width=\(width), height=\(height), scale=\(scale)"
            ).httpResponse
        }
        let scaledWidth = width * scale
        let scaledHeight = height * scale
        guard scaledWidth.isFinite, scaledHeight.isFinite else {
            return AppError(
                message: "Scaled screen dimensions overflowed: width=\(scaledWidth), height=\(scaledHeight)"
            ).httpResponse
        }
        let info = DeviceInfoResponse(
            widthPoints: Int(width),
            heightPoints: Int(height),
            widthPixels: Int(scaledWidth),
            heightPixels: Int(scaledHeight)
        )
        return HttpResponse.json(info)
    }

    static func isScreenStatic() -> HttpResponse {
        let shot1 = XCUIScreen.main.screenshot()
        let shot2 = XCUIScreen.main.screenshot()
        let hash1 = SHA256.hash(data: shot1.pngRepresentation)
        let hash2 = SHA256.hash(data: shot2.pngRepresentation)
        return HttpResponse.json(ScreenStaticResponse(isScreenStatic: hash1 == hash2))
    }

    static func runningApp(_ request: HttpRequest) -> HttpResponse {
        guard let body = try? JSONDecoder().decode(RunningAppRequest.self, from: request.body) else {
            return AppError(type: .precondition, message: "Incorrect request body for runningApp").httpResponse
        }
        let running = body.appIds.first { appId in
            XCUIApplication(bundleIdentifier: appId).state == .runningForeground
        }
        return HttpResponse.json(RunningAppResponse(runningAppBundleId: running ?? RunningApp.springboardBundleId))
    }

    static func keyboard(_ request: HttpRequest) -> HttpResponse {
        guard let body = try? JSONDecoder().decode(KeyboardRequest.self, from: request.body) else {
            return AppError(type: .precondition, message: "Incorrect request body for keyboard").httpResponse
        }
        let appId = RunningApp.getForegroundAppId(body.appIds)
        let isVisible = !appId.isEmpty && XCUIApplication(bundleIdentifier: appId).keyboards.firstMatch.exists
        return HttpResponse.json(KeyboardResponse(isKeyboardVisible: isVisible))
    }

    static func screenshot(_ request: HttpRequest) -> HttpResponse {
        let compressed = request.query["compressed"] == "true"
        let fullScreenshot = XCUIScreen.main.screenshot()
        if compressed {
            guard let jpeg = fullScreenshot.image.jpegData(compressionQuality: 0.5) else {
                return AppError(message: "Failed to encode compressed JPEG screenshot").httpResponse
            }
            return HttpResponse.ok(jpeg, contentType: "image/jpeg")
        }
        return HttpResponse.ok(fullScreenshot.pngRepresentation, contentType: "image/png")
    }
}
