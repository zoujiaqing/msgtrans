# Contributing to MsgTrans

We appreciate your interest in contributing to MsgTrans! This document outlines the guidelines and processes for contributing to this modern multi-protocol communication framework.

## 🌟 Project Vision

MsgTrans aims to make multi-protocol communication simple, efficient, and reliable. Our core principles are:

- **Simplicity**: Elegant APIs that hide complexity
- **Performance**: Lock-free, zero-copy, high-throughput design
- **Reliability**: Type-safe, well-tested, production-ready code
- **Extensibility**: Clean architecture supporting custom protocols
- **Developer Experience**: Excellent documentation and examples

## 🚀 Getting Started

### Prerequisites

- **Rust**: 1.80+ (stable toolchain)
- **Git**: For version control
- **IDE**: VSCode with rust-analyzer or similar

### Development Setup

1. **Fork and Clone**
   ```bash
   git clone https://github.com/zoujiaqing/msgtrans.git
   cd msgtrans-rust
   ```

2. **Install Dependencies**
   ```bash
   cargo build
   ```

3. **Run Tests**
   ```bash
   cargo test --all-features
   ```

4. **Run Examples**
   ```bash
   cargo run --example echo_server
   cargo run --example echo_client_tcp
   ```

## 📋 Types of Contributions

### 🐛 Bug Reports
- Use GitHub Issues with the "bug" label
- Include minimal reproduction steps
- Provide environment details (OS, Rust version)
- Include relevant logs and error messages

### ✨ Feature Requests
- Use GitHub Issues with the "enhancement" label
- Explain the use case and problem it solves
- Consider backward compatibility
- Discuss API design implications

### 📚 Documentation
- README improvements
- API documentation (rustdoc)
- Examples and tutorials
- Architecture documentation

### 🔧 Code Contributions
- Bug fixes
- Performance improvements
- New protocol adapters
- Testing improvements

## 💻 Development Guidelines

### Code Style

**Language Standards**
- All code, comments, and documentation must be in **English**
- No emoji symbols in source code (use text tags like `[PERF]` if needed)
- No "Phase" keywords or development evolution comments
- Professional, technical language throughout

**Rust Standards**
- Follow standard Rust conventions (`cargo fmt`)
- Use `cargo clippy` for linting
- Prefer explicit types when it improves clarity
- Use meaningful variable and function names

**Architecture Patterns**
- Builder pattern for configuration
- Event-driven design
- Protocol-agnostic abstractions
- Zero-copy where possible

### Code Organization

The repository is a Cargo workspace; the library lives in `crates/msgtrans/`:

```
crates/msgtrans/src/
├── lib.rs              # Public API exports
├── error.rs            # Error types and handling
├── event.rs            # Event system definitions
├── packet.rs           # Packet serialization
├── connection.rs       # Connection abstractions
├── transport/          # Transport layer
│   ├── mod.rs
│   ├── server.rs
│   ├── client.rs
│   └── ...
├── adapters/           # Protocol adapters
│   ├── tcp.rs
│   ├── websocket.rs
│   ├── quic.rs
│   └── ...
└── protocol/           # Protocol configurations
    └── ...
```

### Performance Requirements

- **Lock-free**: Use atomic operations and lock-free data structures
- **Zero-copy**: Minimize data copying with `Arc<[u8]>` and similar
- **Async-first**: All I/O must be asynchronous
- **Memory efficient**: Reuse buffers, avoid unnecessary allocations

### Testing Standards

**Unit Tests**
```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_tcp_connection_lifecycle() {
        // Test implementation
    }
}
```

**Integration Tests**
- Place in `tests/` directory
- Test real protocol scenarios
- Include performance benchmarks

**Documentation Tests**
- All public APIs must have doc tests
- Examples should be runnable
- Include error handling examples

## 📝 Commit Guidelines

### Commit Message Format

```
<type>(<scope>): <description>

[optional body]

[optional footer]
```

**Types:**
- `feat`: New feature
- `fix`: Bug fix
- `docs`: Documentation changes
- `style`: Code style changes (formatting, etc.)
- `refactor`: Code refactoring
- `perf`: Performance improvements
- `test`: Adding or updating tests
- `chore`: Build process or auxiliary tool changes

**Examples:**
```
feat(transport): add QUIC protocol adapter

Implement QUIC protocol support with:
- Connection establishment and management
- Stream multiplexing
- TLS 1.3 integration

Closes #123
```

```
fix(tcp): resolve connection leak in error handling

Fix memory leak when TCP connections fail during handshake.
Ensure proper cleanup of file descriptors and allocated buffers.

Fixes #456
```

## 🔄 Pull Request Process

### Before Submitting

1. **Create Feature Branch**
   ```bash
   git checkout -b feature/awesome-new-feature
   ```

2. **Write Tests**
   - Unit tests for new functionality
   - Integration tests for complex features
   - Benchmark tests for performance claims

3. **Update Documentation**
   - Rustdoc for public APIs
   - README if adding new features
   - Examples demonstrating usage

4. **Run Quality Checks**
   ```bash
   cargo fmt
   cargo clippy --all-targets --all-features
   cargo test --all-features
   cargo doc --no-deps
   ```

### Pull Request Template

```markdown
## Summary
Brief description of changes

## Type of Change
- [ ] Bug fix
- [ ] New feature
- [ ] Performance improvement
- [ ] Documentation update
- [ ] Refactoring

## Testing
- [ ] Unit tests pass
- [ ] Integration tests pass
- [ ] Manual testing completed
- [ ] Performance benchmarks (if applicable)

## Documentation
- [ ] Code is documented
- [ ] Examples updated
- [ ] README updated (if needed)

## Checklist
- [ ] Code follows project standards
- [ ] All English language used
- [ ] No emoji symbols in source code
- [ ] Commits follow conventional format
- [ ] Tests added for new functionality
```

### Review Process

1. **Automated Checks**: CI must pass
2. **Code Review**: At least one maintainer approval
3. **Testing**: Comprehensive test coverage
4. **Documentation**: Complete API documentation

## 🧪 Testing

### Running Tests

```bash
# All tests
cargo test --all-features

# Specific module
cargo test transport::tests

# Integration tests
cargo test --test integration

# With output
cargo test -- --nocapture
```

### Performance Testing

```bash
# Benchmarks
cargo bench

# Memory profiling
cargo run --example memory_profile
```

### Coverage

```bash
# Install coverage tool
cargo install cargo-tarpaulin

# Generate coverage report
cargo tarpaulin --all-features --out Html
```

## 📚 Documentation Standards

### API Documentation

```rust
/// Establishes a connection using the specified protocol configuration.
/// 
/// This method performs the complete connection lifecycle including:
/// - Protocol negotiation
/// - Authentication (if required)
/// - Channel establishment
/// 
/// # Arguments
/// 
/// * `config` - Protocol-specific configuration
/// 
/// # Returns
/// 
/// Returns a `Result` containing the established connection or an error.
/// 
/// # Errors
/// 
/// This method will return an error if:
/// - Network connectivity fails
/// - Protocol negotiation fails
/// - Authentication is rejected
/// 
/// # Examples
/// 
/// ```rust
/// use msgtrans::{protocol::TcpClientConfig, tokio};
/// 
/// let config = TcpClientConfig::new("127.0.0.1:8080")?;
/// let client = TransportClientBuilder::new()
///     .with_protocol(config)
///     .build()
///     .await?;
/// ```
pub async fn connect(&mut self) -> Result<(), TransportError> {
    // Implementation
}
```

### Example Standards

- **Complete**: Runnable examples
- **Practical**: Real-world use cases
- **Documented**: Inline comments explaining key concepts
- **Error Handling**: Proper error management

## 🏗️ Adding New Protocol Support

### Implementation Checklist

1. **Create Protocol Adapter**
   ```rust
   pub struct MyProtocolAdapter {
       // Implementation
   }
   
   #[async_trait]
   impl ProtocolAdapter for MyProtocolAdapter {
       // Required methods
   }
   ```

2. **Configuration Types**
   ```rust
   pub struct MyProtocolServerConfig {
       // Configuration fields
   }
   
   impl ServerConfig for MyProtocolServerConfig {
       // Implementation
   }
   ```

3. **Error Handling**
   - Protocol-specific error types
   - Proper error propagation
   - Graceful degradation

4. **Testing Suite**
   - Unit tests for adapter logic
   - Integration tests with real connections
   - Performance benchmarks

5. **Documentation**
   - API documentation
   - Usage examples
   - Protocol-specific guides

## 🚫 Code of Conduct

### Our Standards

- **Respectful**: Treat all contributors with respect
- **Inclusive**: Welcome diverse perspectives and backgrounds
- **Constructive**: Provide helpful, actionable feedback
- **Professional**: Maintain professional communication
- **Collaborative**: Work together toward common goals

### Unacceptable Behavior

- Harassment or discrimination
- Trolling or inflammatory comments
- Personal attacks
- Spam or off-topic content
- Violation of privacy

### Enforcement

Issues can be reported to the maintainers. All reports will be handled confidentially and appropriately.

## 📞 Getting Help

### Communication Channels

- **GitHub Issues**: Bug reports and feature requests
- **GitHub Discussions**: General questions and community discussion
- **Email**: Direct contact with maintainers

### Resources

- **Documentation**: Check rustdoc and README first
- **Examples**: Review example code in `examples/`
- **Tests**: Look at existing tests for patterns

## 🏷️ Release Process

### Versioning

We follow [Semantic Versioning](https://semver.org/):
- **MAJOR**: Breaking API changes
- **MINOR**: New features (backward compatible)
- **PATCH**: Bug fixes (backward compatible)

### Release Checklist

- [ ] All tests pass
- [ ] Documentation updated
- [ ] CHANGELOG.md updated
- [ ] Version bumped in Cargo.toml
- [ ] Git tag created
- [ ] Published to crates.io

## 🙏 Recognition

Contributors are recognized in several ways:
- **Git history**: All contributions are preserved
- **Release notes**: Major contributions highlighted
- **README**: Core contributors listed

Thank you for contributing to MsgTrans! 🚀

---

**Questions?** Open an issue or start a discussion. We're here to help! 