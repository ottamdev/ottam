import OttamCore
import Foundation

// Hand-written protocol infrastructure. Wire-shape types are generated from
// the Rust protocol crate into ProtocolTypes.generated.swift; what stays here
// is deliberately not derivable from the schema: the JSON-RPC envelopes,
// `RequestID` and `JSONValue`.

public enum RequestID: Hashable, Codable, Sendable {
    case null
    case number(Int64)
    case string(String)

    public init(from decoder: Decoder) throws {
        let container = try decoder.singleValueContainer()
        if container.decodeNil() {
            self = .null
        } else if let number = try? container.decode(Int64.self) {
            self = .number(number)
        } else {
            self = .string(try container.decode(String.self))
        }
    }

    public func encode(to encoder: Encoder) throws {
        var container = encoder.singleValueContainer()
        switch self {
        case .null:
            try container.encodeNil()
        case .number(let number):
            try container.encode(number)
        case .string(let string):
            try container.encode(string)
        }
    }
}

public struct ProtocolRequest<Params: Encodable>: Encodable {
    public var jsonrpc: String?
    public var id: RequestID
    public var method: String
    public var params: Params

    public init(jsonrpc: String? = nil, id: RequestID, method: String, params: Params) {
        self.jsonrpc = jsonrpc
        self.id = id
        self.method = method
        self.params = params
    }

    private enum CodingKeys: String, CodingKey {
        case jsonrpc
        case id
        case method
        case params
    }

    public func encode(to encoder: Encoder) throws {
        var container = encoder.container(keyedBy: CodingKeys.self)
        try container.encodeIfPresent(jsonrpc, forKey: .jsonrpc)
        try container.encode(id, forKey: .id)
        try container.encode(method, forKey: .method)
        try container.encode(params, forKey: .params)
    }
}

public struct ProtocolNotification<Params: Codable>: Codable {
    public var jsonrpc: String?
    public var method: String
    public var params: Params

    public init(jsonrpc: String? = nil, method: String, params: Params) {
        self.jsonrpc = jsonrpc
        self.method = method
        self.params = params
    }

    private enum CodingKeys: String, CodingKey {
        case jsonrpc
        case method
        case params
    }

    public func encode(to encoder: Encoder) throws {
        var container = encoder.container(keyedBy: CodingKeys.self)
        try container.encodeIfPresent(jsonrpc, forKey: .jsonrpc)
        try container.encode(method, forKey: .method)
        try container.encode(params, forKey: .params)
    }
}

public struct ProtocolResponse<Result: Decodable>: Decodable {
    public var jsonrpc: String?
    public var id: RequestID
    public var result: Result?
    public var error: AgentProtocolError?

    private enum CodingKeys: String, CodingKey {
        case jsonrpc
        case id
        case result
        case error
    }

    public init(from decoder: Decoder) throws {
        let container = try decoder.container(keyedBy: CodingKeys.self)
        jsonrpc = try container.decodeIfPresent(String.self, forKey: .jsonrpc)
        id = try container.decode(RequestID.self, forKey: .id)
        result = try container.decodeIfPresent(Result.self, forKey: .result)
        error = try container.decodeIfPresent(AgentProtocolError.self, forKey: .error)

        switch (result != nil, error != nil) {
        case (true, true):
            throw DecodingError.dataCorrupted(
                DecodingError.Context(
                    codingPath: decoder.codingPath,
                    debugDescription: "protocol response must not contain both result and error"
                )
            )
        case (false, false):
            throw DecodingError.dataCorrupted(
                DecodingError.Context(
                    codingPath: decoder.codingPath,
                    debugDescription: "protocol response must contain exactly one of result or error"
                )
            )
        default:
            break
        }
    }
}

public struct AgentNotification: Sendable {
    public var method: String
    public var payload: Data

    public init(method: String, payload: Data) {
        self.method = method
        self.payload = payload
    }
}

public enum JSONValue: Codable, Equatable, Sendable {
    case null
    case bool(Bool)
    case number(Double)
    case string(String)
    case array([JSONValue])
    case object([String: JSONValue])

    public init(from decoder: Decoder) throws {
        let container = try decoder.singleValueContainer()
        if container.decodeNil() {
            self = .null
        } else if let value = try? container.decode(Bool.self) {
            self = .bool(value)
        } else if let value = try? container.decode(Double.self) {
            self = .number(value)
        } else if let value = try? container.decode(String.self) {
            self = .string(value)
        } else if let value = try? container.decode([JSONValue].self) {
            self = .array(value)
        } else {
            self = .object(try container.decode([String: JSONValue].self))
        }
    }

    public func encode(to encoder: Encoder) throws {
        var container = encoder.singleValueContainer()
        switch self {
        case .null:
            try container.encodeNil()
        case .bool(let value):
            try container.encode(value)
        case .number(let value):
            try container.encode(value)
        case .string(let value):
            try container.encode(value)
        case .array(let value):
            try container.encode(value)
        case .object(let value):
            try container.encode(value)
        }
    }
}
