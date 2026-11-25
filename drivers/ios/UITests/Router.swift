// Dispatches the HTTP routes consumed by `crates/tapwing/src/ios_native_lane/client.rs`.
// `GET /status` is not here: `HttpServer` answers it off the main thread, so it
// keeps working while a handler below is wedged.

import Foundation

@MainActor
final class Router {
    private let hierarchy = HierarchyBuilder()

    /// `ticket` lets long-running handlers stop once their request is dead.
    func handle(_ request: HttpRequest, ticket: RequestTicket) async -> HttpResponse {
        switch (request.method, request.path) {
        case ("GET", "/deviceInfo"):
            return ScreenProbe.deviceInfo()
        case ("POST", "/viewHierarchy"):
            return await hierarchy.handle(request)
        case ("POST", "/touch"):
            return await Actions.touch(request)
        case ("POST", "/swipeV2"):
            return await Actions.swipeV2(request)
        case ("POST", "/inputText"):
            return await Actions.inputText(request, ticket: ticket)
        case ("POST", "/eraseText"):
            return await Actions.eraseText(request, ticket: ticket)
        case ("POST", "/pressKey"):
            return await Actions.pressKey(request)
        case ("POST", "/pressButton"):
            return Actions.pressButton(request)
        case ("POST", "/setOrientation"):
            return Actions.setOrientation(request)
        case ("GET", "/isScreenStatic"):
            return ScreenProbe.isScreenStatic()
        case ("POST", "/runningApp"):
            return ScreenProbe.runningApp(request)
        case ("POST", "/keyboard"):
            return ScreenProbe.keyboard(request)
        case ("GET", "/screenshot"):
            return ScreenProbe.screenshot(request)
        default:
            return AppError(type: .notFound, message: "Unknown route \(request.method) \(request.path)").httpResponse
        }
    }
}
