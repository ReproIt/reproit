import Foundation

/// Foundation URL loading adapter used only under a ReproIt run. It captures
/// through URLProtocol, logs the universal redacted exchange marker, and serves
/// exact capsule responses without touching the live backend.
final class ReproItCausalURLProtocol: URLProtocol {
    private static let lock = NSLock()
    private static var actionIndex = 0
    private static var ordinal = 0
    private static var used = Set<Int>()
    private static var exchanges: [[String: Any]] = []
    private static var excludePrefix: String?
    private static var installed = false
    private var loadingTask: URLSessionDataTask?

    static func install(excluding endpoint: String?) {
        guard !installed, ProcessInfo.processInfo.environment["REPROIT_CAUSAL"] == "1" else { return }
        installed = true
        excludePrefix = endpoint
        if let raw = ProcessInfo.processInfo.environment["REPROIT_CAPSULE_JSON"],
           let data = raw.data(using: .utf8),
           let root = try? JSONSerialization.jsonObject(with: data) as? [String: Any] {
            exchanges = root["exchanges"] as? [[String: Any]] ?? []
        }
        URLProtocol.registerClass(Self.self)
        let capabilities = "{\"http\":{\"status\":\"captured\"},\"http_replay\":{\"status\":\"captured\"}}"
        NSLog("REPROIT:CAPABILITIES %@", capabilities)
        mergeCapabilities(capabilities)
    }

    static func advanceAction() {
        lock.lock(); actionIndex += 1; ordinal = 0; lock.unlock()
    }

    override class func canInit(with request: URLRequest) -> Bool {
        guard installed, URLProtocol.property(forKey: "ReproItHandled", in: request) == nil,
              let scheme = request.url?.scheme?.lowercased(), scheme == "http" || scheme == "https" else { return false }
        if let prefix = excludePrefix, request.url?.absoluteString.hasPrefix(prefix) == true { return false }
        return true
    }

    override class func canonicalRequest(for request: URLRequest) -> URLRequest { request }

    override func startLoading() {
        guard let url = request.url else { return }
        Self.lock.lock()
        let action = Self.actionIndex
        let currentOrdinal = Self.ordinal
        Self.ordinal += 1
        let replaying = !Self.exchanges.isEmpty
        var matched: (Int, [String: Any])?
        if replaying {
            for (index, exchange) in Self.exchanges.enumerated() where !Self.used.contains(index) {
                if exchange["required"] as? Bool == true,
                   exchange["actor"] as? String == (ProcessInfo.processInfo.environment["REPROIT_DEVICE"] ?? "a"),
                   exchange["actionIndex"] as? Int == action,
                   (exchange["method"] as? String)?.uppercased() == (request.httpMethod ?? "GET").uppercased(),
                   Self.canonical(exchange["url"] as? String) == Self.canonical(url.absoluteString) {
                    matched = (index, exchange); break
                }
            }
        }
        if let (index, exchange) = matched {
            Self.used.insert(index)
            Self.lock.unlock()
            let status = exchange["status"] as? Int ?? 200
            let headers = exchange["responseHeaders"] as? [String: String] ?? [:]
            let body = exchange["responseBody"]
            let data: Data
            if let string = body as? String { data = Data(string.utf8) }
            else { data = (try? JSONSerialization.data(withJSONObject: body ?? "")) ?? Data() }
            let response = HTTPURLResponse(url: url, statusCode: status, httpVersion: "HTTP/1.1", headerFields: headers)!
            client?.urlProtocol(self, didReceive: response, cacheStoragePolicy: .notAllowed)
            client?.urlProtocol(self, didLoad: data)
            client?.urlProtocolDidFinishLoading(self)
            NSLog("CAPSULE:HIT %@", exchange["id"] as? String ?? "")
            return
        }
        Self.lock.unlock()
        if replaying {
            let message = "CAPSULE:MISS \(request.httpMethod ?? "GET") \(url.absoluteString) action=\(action)"
            NSLog("%@", message)
            client?.urlProtocol(self, didFailWithError: NSError(domain: "ReproItCapsule", code: 1, userInfo: [NSLocalizedDescriptionKey: message]))
            return
        }

        let mutable = (request as NSURLRequest).mutableCopy() as! NSMutableURLRequest
        URLProtocol.setProperty(true, forKey: "ReproItHandled", in: mutable)
        let config = URLSessionConfiguration.ephemeral
        config.protocolClasses = []
        loadingTask = URLSession(configuration: config).dataTask(with: mutable as URLRequest) { data, response, error in
            if let error { self.client?.urlProtocol(self, didFailWithError: error); return }
            guard let response = response as? HTTPURLResponse else { return }
            let data = data ?? Data()
            let requestHeaders = Self.redactHeaders(self.request.allHTTPHeaderFields ?? [:])
            let responseHeaders = Self.redactHeaders(response.allHeaderFields.reduce(into: [String: String]()) { $0[String(describing: $1.key)] = String(describing: $1.value) })
            let exchange: [String: Any] = [
                "id": "\(ProcessInfo.processInfo.environment["REPROIT_DEVICE"] ?? "a")-\(action)-\(currentOrdinal)",
                "actor": ProcessInfo.processInfo.environment["REPROIT_DEVICE"] ?? "a",
                "actionIndex": action, "ordinal": currentOrdinal, "protocol": url.scheme ?? "http",
                "method": self.request.httpMethod ?? "GET", "url": url.absoluteString,
                "requestHeaders": requestHeaders,
                "requestBody": Self.bodyValue(self.request.httpBody, headers: requestHeaders),
                "status": response.statusCode, "responseHeaders": responseHeaders,
                "responseBody": Self.bodyValue(data, headers: responseHeaders), "required": true,
            ]
            if let raw = try? JSONSerialization.data(withJSONObject: exchange), let line = String(data: raw, encoding: .utf8) {
                NSLog("REPROIT:EXCHANGE %@", line)
                Self.appendExchange(line)
            }
            self.client?.urlProtocol(self, didReceive: response, cacheStoragePolicy: .notAllowed)
            self.client?.urlProtocol(self, didLoad: data)
            self.client?.urlProtocolDidFinishLoading(self)
        }
        loadingTask?.resume()
    }

    override func stopLoading() { loadingTask?.cancel() }

    static func secret(_ key: String) -> Bool {
        let key = key.lowercased().filter { !"-_. ".contains($0) }
        return ["password", "passwd", "secret", "token", "authorization", "cookie", "email", "phone",
                "apikey", "publishablekey", "privatekey", "accesskey", "signingkey"].contains { key.contains($0) }
    }
    private static func appendExchange(_ line: String) {
        guard let path = ProcessInfo.processInfo.environment["REPROIT_NETWORK_FILE"] else { return }
        guard let data = (line + "\n").data(using: .utf8) else { return }
        if !FileManager.default.fileExists(atPath: path) { FileManager.default.createFile(atPath: path, contents: nil) }
        guard let handle = try? FileHandle(forWritingTo: URL(fileURLWithPath: path)) else { return }
        defer { try? handle.close() }
        do { try handle.seekToEnd(); try handle.write(contentsOf: data) } catch { }
    }
    private static func mergeCapabilities(_ raw: String) {
        guard let path = ProcessInfo.processInfo.environment["REPROIT_CAPABILITIES_FILE"],
              let update = raw.data(using: .utf8),
              let incoming = try? JSONSerialization.jsonObject(with: update) as? [String: Any] else { return }
        var existing: [String: Any] = [:]
        if let data = try? Data(contentsOf: URL(fileURLWithPath: path)),
           let value = try? JSONSerialization.jsonObject(with: data) as? [String: Any] { existing = value }
        incoming.forEach { existing[$0.key] = $0.value }
        if let data = try? JSONSerialization.data(withJSONObject: existing) { try? data.write(to: URL(fileURLWithPath: path), options: .atomic) }
    }
    private static func canonical(_ raw: String?) -> String {
        guard let raw, var components = URLComponents(string: raw) else { return raw ?? "" }
        components.scheme = components.scheme?.lowercased()
        components.host = components.host?.lowercased()
        if let items = components.queryItems {
            components.queryItems = items.sorted {
                if $0.name != $1.name { return $0.name < $1.name }
                return ($0.value ?? "") < ($1.value ?? "")
            }
        }
        return components.string ?? raw
    }
    private static func redactHeaders(_ headers: [String: String]) -> [String: String] {
        Dictionary(uniqueKeysWithValues: headers.map { ($0.key, secret($0.key) ? "<reproit:secret>" : $0.value) })
    }
    private static func bodyValue(_ data: Data?, headers: [String: String]) -> Any {
        guard let data, !data.isEmpty else { return NSNull() }
        if headers.first(where: { $0.key.lowercased() == "content-type" })?.value.contains("json") == true,
           let value = try? JSONSerialization.jsonObject(with: data) { return redact(value) }
        return "<reproit:body:length=\(data.count)>"
    }
    static func redact(_ value: Any) -> Any {
        if let values = value as? [Any] { return values.map(redact) }
        if let map = value as? [String: Any] {
            var result: [String: Any] = [:]
            for (key, child) in map {
                if secret(key) {
                    result[key] = child is String
                        ? "<reproit:string:length=\((child as! String).count)>"
                        : "<reproit:secret>"
                } else {
                    result[key] = redact(child)
                }
            }
            return result
        }
        return value
    }
}
