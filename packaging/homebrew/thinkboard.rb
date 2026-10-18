# ThinkBoard for Homebrew: `brew install vestigecounty/tap/thinkboard`.
#
# Written into vestigecounty/homebrew-tap by ThinkBoard's release workflow,
# from packaging/homebrew/thinkboard.rb in vestigecounty/thinkboard, with
# each release's version and checksum in place of the @-names. Edit it there:
# the tap's copy is replaced on every release.
class Thinkboard < Formula
  desc "Your Obsidian canvas, over everything"
  homepage "https://vestigecounty.github.io/thinkboard/"
  url "https://github.com/vestigecounty/thinkboard/releases/download/v@VERSION@/thinkboard-@VERSION@-macos-universal.zip"
  sha256 "@SHA256@"
  license "MIT"

  # The release's universal .app — one download for Apple silicon and Intel.
  # Linux has a tarball of its own, with its own installer.
  depends_on :macos

  def install
    # Homebrew steps into an archive's lone folder, so what's here is the
    # bundle's insides. It's installed whole, not just its binary, so that
    # it can be opened as the app it is — with its icon, and its Info.plist.
    (prefix/"ThinkBoard.app").install "Contents"
    bin.write_exec_script prefix/"ThinkBoard.app/Contents/MacOS/thinkboard"
  end

  # Homebrew follows this with how to start the service, and how to run the
  # binary instead — which ties it to the terminal it's run in.
  def caveats
    <<~EOS
      To open ThinkBoard once, as an app:
        open #{opt_prefix}/ThinkBoard.app
    EOS
  end

  # Started at login and left be: no KeepAlive, so Quit in the tray menu
  # quits, rather than launchd bringing it straight back.
  service do
    run opt_prefix/"ThinkBoard.app/Contents/MacOS/thinkboard"
    # Scheduled as an app, not as a background job: it's summoned by a
    # hotkey and has to come up at once.
    process_type :interactive
  end

  test do
    # `--help` prints and exits before anything else — before it would
    # replace a running ThinkBoard, or open a window.
    assert_match "Usage:", shell_output("#{bin}/thinkboard --help")
  end
end
