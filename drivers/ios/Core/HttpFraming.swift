// Minimal zero-dependency HTTP/1.1 framing, request parsing, and response
// serialization for Tapwing's iOS Simulator XCUITest runner.
//
// Designed for single-client keep-alive traffic from `ureq::Agent` over
// loopback (`127.0.0.1`). Supports `Content-Length` framing, query parameters,
// and persistent HTTP/1.1 connections without external Swift packages.

import Foundation

public struct HttpRequest: Equatable, Sendable {
    public let method: String
    public let target: String
    public let path: String
    public let query: [String: String]
    public let httpVersion: String
    public let headers: [String: String] // lowercased keys
    public let body: Data

    public init(
        method: String,
        target: String,
        path: String,
        query: [String: String],
        httpVersion: String,
        headers: [String: String],
        body: Data
    ) {
        self.method = method
        self.target = target
        self.path = path
        self.query = query
        self.httpVersion = httpVersion
        self.headers = headers
        self.body = body
    }

    /// True if the TCP connection should stay open after replying.
    public var shouldKeepAlive: Bool {
        if let conn = headers["connection"]?.lowercased() {
            let tokens = conn.split(separator: ",").map { $0.trimmingCharacters(in: .whitespaces) }
            if tokens.contains("close") {
                return false
            }
            if tokens.contains("keep-alive") {
                return true
            }
        }
        return httpVersion != "HTTP/1.0"
    }
}

public struct HttpResponse: Equatable, Sendable {
    public let statusCode: Int
    public let reasonPhrase: String
    public let contentType: String
    public let body: Data

    public init(
        statusCode: Int,
        reasonPhrase: String? = nil,
        contentType: String = "application/json",
        body: Data = Data()
    ) {
        self.statusCode = statusCode
        self.reasonPhrase = reasonPhrase ?? HttpFraming.reasonPhrase(for: statusCode)
        self.contentType = contentType
        self.body = body
    }

    public static func ok(_ body: Data = Data("{}".utf8), contentType: String = "application/json") -> HttpResponse {
        HttpResponse(statusCode: 200, contentType: contentType, body: body)
    }

    public static func json<T: Encodable>(_ value: T, statusCode: Int = 200) -> HttpResponse {
        do {
            let data = try JSONEncoder().encode(value)
            return HttpResponse(statusCode: statusCode, contentType: "application/json", body: data)
        } catch {
            return AppError(message: "Failed to encode JSON response: \(error.localizedDescription)").httpResponse
        }
    }
}

public enum HttpParseResult: Equatable, Sendable {
    case incomplete
    case complete(HttpRequest)
    case malformed(String)
}

public enum HttpFraming {
    /// Safety cap on a single HTTP request header block (64 KiB).
    public static let maxHeaderBytes = 64 * 1024
    /// Safety cap on a single HTTP request body (16 MiB).
    public static let maxBodyBytes = 16 * 1024 * 1024

    private static let headerTerminator = Data([0x0D, 0x0A, 0x0D, 0x0A]) // \r\n\r\n

    /// Attempts to consume one complete HTTP/1.1 request from the front of `buffer`.
    /// Leaves any trailing bytes (pipelined next request) intact in `buffer`.
    public static func tryParseRequest(from buffer: inout Data) -> HttpParseResult {
        guard let separatorRange = buffer.range(of: headerTerminator) else {
            if buffer.count > maxHeaderBytes {
                return .malformed("HTTP headers exceed \(maxHeaderBytes) bytes")
            }
            return .incomplete
        }

        let headerSlice = buffer[buffer.startIndex..<separatorRange.lowerBound]
        if headerSlice.count > maxHeaderBytes {
            return .malformed("HTTP headers exceed \(maxHeaderBytes) bytes")
        }

        guard let headerText = String(data: headerSlice, encoding: .utf8) else {
            return .malformed("HTTP headers are not valid UTF-8")
        }

        let lines = headerText.components(separatedBy: "\r\n")
        guard let requestLine = lines.first, !requestLine.isEmpty else {
            return .malformed("Empty HTTP request line")
        }

        let parts = requestLine.split(separator: " ", omittingEmptySubsequences: true)
        guard parts.count >= 2 else {
            return .malformed("Malformed HTTP request line: \(requestLine)")
        }

        let method = String(parts[0]).uppercased()
        let target = String(parts[1])
        let httpVersion = parts.count >= 3 ? String(parts[2]).uppercased() : "HTTP/1.1"

        var headers: [String: String] = [:]
        headers.reserveCapacity(max(lines.count - 1, 0))
        for line in lines.dropFirst() where !line.isEmpty {
            guard let colon = line.firstIndex(of: ":") else {
                return .malformed("Malformed HTTP header line: \(line)")
            }
            let name = line[..<colon].trimmingCharacters(in: .whitespaces).lowercased()
            let value = line[line.index(after: colon)...].trimmingCharacters(in: .whitespaces)
            if let existing = headers[name] {
                headers[name] = "\(existing), \(value)"
            } else {
                headers[name] = value
            }
        }

        if let te = headers["transfer-encoding"], te.lowercased().contains("chunked") {
            return .malformed("Chunked request Transfer-Encoding is not supported")
        }

        let contentLength: Int
        if let rawLen = headers["content-length"] {
            guard let parsed = Int(rawLen), parsed >= 0 else {
                return .malformed("Invalid Content-Length: \(rawLen)")
            }
            if parsed > maxBodyBytes {
                return .malformed("Request body exceeds \(maxBodyBytes) bytes")
            }
            contentLength = parsed
        } else {
            contentLength = 0
        }

        let bodyStart = separatorRange.upperBound
        let availableBody = buffer.distance(from: bodyStart, to: buffer.endIndex)
        guard availableBody >= contentLength else {
            return .incomplete
        }

        let bodyEnd = buffer.index(bodyStart, offsetBy: contentLength)
        let body = Data(buffer[bodyStart..<bodyEnd])
        buffer.removeSubrange(buffer.startIndex..<bodyEnd)

        let (path, query) = splitTarget(target)
        return .complete(
            HttpRequest(
                method: method,
                target: target,
                path: path,
                query: query,
                httpVersion: httpVersion,
                headers: headers,
                body: body
            )
        )
    }

    /// Serializes an HTTP/1.1 response with explicit `Content-Length` and `Connection`.
    public static func serialize(_ response: HttpResponse, keepAlive: Bool) -> Data {
        let connection = keepAlive ? "keep-alive" : "close"
        let header =
            "HTTP/1.1 \(response.statusCode) \(response.reasonPhrase)\r\n" +
            "Content-Type: \(response.contentType)\r\n" +
            "Content-Length: \(response.body.count)\r\n" +
            "Connection: \(connection)\r\n" +
            "\r\n"
        var out = Data(header.utf8)
        out.append(response.body)
        return out
    }

    /// Splits `/screenshot?compressed=false&foo=bar` into path and decoded query dictionary.
    public static func splitTarget(_ target: String) -> (path: String, query: [String: String]) {
        guard let qIndex = target.firstIndex(of: "?") else {
            return (target, [:])
        }
        let path = String(target[..<qIndex])
        let queryString = target[target.index(after: qIndex)...]
        var query: [String: String] = [:]
        for pair in queryString.split(separator: "&", omittingEmptySubsequences: true) {
            if let eq = pair.firstIndex(of: "=") {
                let rawKey = String(pair[..<eq])
                let rawVal = String(pair[pair.index(after: eq)...])
                let key = rawKey.removingPercentEncoding ?? rawKey
                let val = rawVal.removingPercentEncoding ?? rawVal
                query[key] = val
            } else {
                let rawKey = String(pair)
                query[rawKey.removingPercentEncoding ?? rawKey] = ""
            }
        }
        return (path, query)
    }

    public static func reasonPhrase(for statusCode: Int) -> String {
        switch statusCode {
        case 200: return "OK"
        case 400: return "Bad Request"
        case 404: return "Not Found"
        case 405: return "Method Not Allowed"
        case 408: return "Request Timeout"
        case 500: return "Internal Server Error"
        default: return "Status"
        }
    }
}
