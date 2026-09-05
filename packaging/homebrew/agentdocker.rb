# Homebrew formula for a tap (e.g. brandopakel/homebrew-agentdocker).
# Update `version` and the four sha256 values from the release's .sha256
# files; `brew install brandopakel/agentdocker/agentdocker` then installs
# both binaries and `brew services start agentdocker` runs the daemon.
class Agentdocker < Formula
  desc "Docker-style control plane for AI agents"
  homepage "https://github.com/brandopakel/AgentDocker"
  version "0.1.0"
  license "MIT"

  base = "https://github.com/brandopakel/AgentDocker/releases/download/v#{version}"

  on_macos do
    on_arm do
      url "#{base}/agentdocker-aarch64-apple-darwin.tar.gz"
      sha256 "0000000000000000000000000000000000000000000000000000000000000000"
    end
    on_intel do
      url "#{base}/agentdocker-x86_64-apple-darwin.tar.gz"
      sha256 "0000000000000000000000000000000000000000000000000000000000000000"
    end
  end

  on_linux do
    on_arm do
      url "#{base}/agentdocker-aarch64-unknown-linux-musl.tar.gz"
      sha256 "0000000000000000000000000000000000000000000000000000000000000000"
    end
    on_intel do
      url "#{base}/agentdocker-x86_64-unknown-linux-musl.tar.gz"
      sha256 "0000000000000000000000000000000000000000000000000000000000000000"
    end
  end

  def install
    bin.install "agentdocker", "agentd"
  end

  service do
    run [opt_bin/"agentd"]
    keep_alive successful_exit: false
    log_path var/"log/agentd.log"
    error_log_path var/"log/agentd.log"
  end

  test do
    assert_match "agentdocker", shell_output("#{bin}/agentdocker --version")
  end
end
