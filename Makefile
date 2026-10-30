LOCAL_AGENT_PACKAGE := local_multi_agent_service
LOCAL_AGENT_BIN := warp-local-multi-agent
RELEASE_REMOTE ?= origin
REF ?= HEAD
DRY_RUN ?= 0

.DEFAULT_GOAL := help

.PHONY: help local-agent-install local-agent-dev local-agent-build local-agent-start local-agent-test release-macos warp-local-signing-identity warp-signing-status warp-grant-keychain-access warp-trash-local-settings warp-check warp-build warp-build-optimized warp-build-oss

help:
	@echo "Slipstream local development targets:"
	@echo "  make local-agent-install  Fetch local multi-agent service dependencies"
	@echo "  make local-agent-dev      Run the local multi-agent service"
	@echo "  make local-agent-build    Build the local multi-agent service"
	@echo "  make local-agent-start    Run the built local multi-agent service"
	@echo "  make local-agent-test     Build and test the local multi-agent service"
	@echo "  make release-macos TAG=v0.2.0 REF=<commit-ish>  Tag a commit and trigger the macOS + SSH extension release workflow"
	@echo "  make warp-local-signing-identity  Create a stable local macOS signing identity"
	@echo "  make warp-signing-status  Show available macOS code-signing identities"
	@echo "  make warp-grant-keychain-access  Grant existing Slipstream keychain items to the signed app Team ID"
	@echo "  make warp-trash-local-settings  Move local Slipstream settings/state to the macOS Trash"
	@echo "  make warp-check           Run Rust formatting and Slipstream app check"
	@echo "  make warp-build-oss       Build the Slipstream macOS app bundle"
	@echo "  make warp-build-optimized Build an optimized Slipstream macOS app bundle"

local-agent-install:
	cargo fetch

local-agent-dev:
	cargo run -p $(LOCAL_AGENT_PACKAGE) --bin $(LOCAL_AGENT_BIN)

local-agent-build:
	cargo build -p $(LOCAL_AGENT_PACKAGE) --bin $(LOCAL_AGENT_BIN)

local-agent-start: local-agent-build
	./target/debug/$(LOCAL_AGENT_BIN)

local-agent-test:
	cargo test -p $(LOCAL_AGENT_PACKAGE)

release-macos:
	@bash -eu -o pipefail -c '\
		tag="$$1"; \
		ref="$$2"; \
		remote="$$3"; \
		dry_run="$$4"; \
		if [[ -z "$$tag" ]]; then \
			echo "Usage: make release-macos TAG=v0.2.0 REF=<commit-ish>"; \
			exit 2; \
		fi; \
		if ! [[ "$$tag" =~ ^v[0-9]+\.[0-9]+\.[0-9]+$$ ]]; then \
			echo "Release tag must match vMAJOR.MINOR.PATCH, got: $$tag"; \
			exit 2; \
		fi; \
		git fetch "$$remote" --tags >/dev/null; \
		if ! commit="$$(git rev-parse --verify "$$ref^{commit}" 2>/dev/null)"; then \
			echo "Could not resolve REF to a commit: $$ref"; \
			exit 2; \
		fi; \
		if git show-ref --verify --quiet "refs/tags/$$tag"; then \
			echo "Tag already exists locally or was fetched from $$remote: $$tag"; \
			exit 2; \
		fi; \
		if git ls-remote --exit-code --tags "$$remote" "refs/tags/$$tag" >/dev/null 2>&1; then \
			echo "Tag already exists on $$remote: $$tag"; \
			exit 2; \
		fi; \
		subject="$$(git log -1 --format=%s "$$commit")"; \
		echo "Release tag: $$tag"; \
		echo "Release commit: $$commit"; \
		echo "Commit subject: $$subject"; \
		echo "Remote: $$remote"; \
		if [[ "$$dry_run" == "1" || "$$dry_run" == "true" ]]; then \
			echo "DRY_RUN=$$dry_run, not creating or pushing the tag."; \
			exit 0; \
		fi; \
		git tag -a "$$tag" "$$commit" -m "Slipstream $$tag"; \
		git push "$$remote" "refs/tags/$$tag"; \
		echo "Triggered macOS release workflow for $$tag."; \
		echo "Watch with: gh run list --workflow release-macos.yml --branch $$tag --limit 1"; \
	' -- "$(TAG)" "$(REF)" "$(RELEASE_REMOTE)" "$(DRY_RUN)"

warp-local-signing-identity:
	./script/macos/create_local_codesign_identity

warp-signing-status:
	./script/macos/signing_status

warp-grant-keychain-access:
	./script/macos/grant_keychain_access_to_team

warp-trash-local-settings:
	@bash -eu -o pipefail -c '\
		if [ "$$(uname -s)" != "Darwin" ]; then \
			echo "warp-trash-local-settings currently supports macOS only."; \
			exit 1; \
		fi; \
		ts=$$(date +%Y%m%d-%H%M%S); \
		trash_dir="$$HOME/.Trash/slipstream-local-settings-$$ts"; \
		mkdir -p "$$trash_dir"; \
		paths=( \
			"$$HOME/.slipstream" \
			"$$HOME/Library/Application Support/com.slipstream.app" \
			"$$HOME/Library/Caches/com.slipstream.app" \
			"$$HOME/Library/Preferences/com.slipstream.app.plist" \
			"$$HOME/Library/Saved Application State/com.slipstream.app.savedState" \
		); \
		services=( "com.slipstream.app" ); \
		if [ -n "$${WARP_DATA_PROFILE:-}" ]; then \
			paths+=( \
				"$$HOME/.slipstream-$${WARP_DATA_PROFILE}" \
				"$$HOME/Library/Application Support/com.slipstream.app-$${WARP_DATA_PROFILE}" \
				"$$HOME/Library/Caches/com.slipstream.app-$${WARP_DATA_PROFILE}" \
				"$$HOME/Library/Preferences/com.slipstream.app-$${WARP_DATA_PROFILE}.plist" \
				"$$HOME/Library/Saved Application State/com.slipstream.app-$${WARP_DATA_PROFILE}.savedState" \
			); \
			services+=( "com.slipstream.app-$${WARP_DATA_PROFILE}" ); \
		fi; \
		if [ -n "$${SLIPSTREAM_APP_GROUP_ID:-}" ]; then \
			paths+=( "$$HOME/Library/Group Containers/$${SLIPSTREAM_APP_GROUP_ID}/Library/Application Support/com.slipstream.app" ); \
		fi; \
		moved=0; \
		for path in "$${paths[@]}"; do \
			if [ -e "$$path" ]; then \
				dest="$$trash_dir$${path#$$HOME}"; \
				mkdir -p "$$(dirname "$$dest")"; \
				echo "Moving $$path"; \
				mv "$$path" "$$dest"; \
				moved=1; \
			fi; \
		done; \
		for service in "$${services[@]}"; do \
			for key in User AiApiKeys McpCredentials TemplatableMcpCredentials FileBasedMcpCredentials; do \
				security delete-generic-password -s "$$service" -a "$$key" >/dev/null 2>&1 || true; \
			done; \
		done; \
		defaults delete com.slipstream.app >/dev/null 2>&1 || true; \
		killall cfprefsd >/dev/null 2>&1 || true; \
		if [ "$$moved" -eq 0 ]; then \
			rmdir "$$trash_dir"; \
			echo "No local Slipstream settings/state paths found."; \
		else \
			echo "Moved local Slipstream settings/state to $$trash_dir"; \
		fi; \
		echo "Deleted Slipstream Keychain entries for user, API key, and MCP credentials if present."; \
	'

warp-check:
	cargo fmt --check
	cargo check -p warp --bin warp-oss --features gui

warp-build:
	PATH="$$HOME/.cargo/bin:$$PATH" TERM=xterm-256color FEATURES=gui WARP_BIN_NAME=warp-oss WARP_CHANNEL=oss ./script/macos/run --dont-open

warp-build-optimized:
	PATH="$$HOME/.cargo/bin:$$PATH" TERM=xterm-256color FEATURES=gui WARP_BIN_NAME=warp-oss WARP_CHANNEL=oss CARGO_PROFILE_RELEASE_SPLIT_DEBUGINFO=unpacked ./script/macos/run --dont-open --profile release-lto

warp-build-oss: warp-build
