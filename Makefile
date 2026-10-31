LOCAL_AGENT_PACKAGE := local_multi_agent_service
LOCAL_AGENT_BIN := warp-local-multi-agent

.DEFAULT_GOAL := help

.PHONY: help local-agent-install local-agent-dev local-agent-build local-agent-start local-agent-test warp-check warp-build warp-build-optimized warp-build-oss warp-run warp-trash-local-settings

help:
	@echo "Slipstream local development targets:"
	@echo "  make local-agent-install  Fetch local multi-agent service dependencies"
	@echo "  make local-agent-dev      Run the local multi-agent service"
	@echo "  make local-agent-build    Build the local multi-agent service"
	@echo "  make local-agent-start    Run the built local multi-agent service"
	@echo "  make local-agent-test     Build and test the local multi-agent service"
	@echo "  make warp-check           Run Rust formatting and Slipstream app check"
	@echo "  make warp-build-oss       Build a debug Slipstream app bundle"
	@echo "  make warp-build-optimized Build a release-lto Slipstream app bundle"
	@echo "  make warp-run             Build and run Slipstream without bundling"
	@echo "  make warp-trash-local-settings  Remove local Slipstream settings/state"

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

warp-trash-local-settings:
	@bash -eu -o pipefail -c '\
		ts=$$(date +%Y%m%d-%H%M%S); \
		trash_dir="$${XDG_CACHE_HOME:-$$HOME/.cache}/slipstream-trashed-settings-$$ts"; \
		mkdir -p "$$trash_dir"; \
		paths=( \
			"$$HOME/.slipstream" \
			"$${XDG_CONFIG_HOME:-$$HOME/.config}/dev.warp.WarpOss" \
			"$${XDG_DATA_HOME:-$$HOME/.local/share}/dev.warp.WarpOss" \
			"$${XDG_STATE_HOME:-$$HOME/.local/state}/dev.warp.WarpOss" \
			"$${XDG_CACHE_HOME:-$$HOME/.cache}/dev.warp.WarpOss" \
		); \
		if [ -n "$${WARP_DATA_PROFILE:-}" ]; then \
			paths+=( \
				"$$HOME/.slipstream-$${WARP_DATA_PROFILE}" \
				"$${XDG_CONFIG_HOME:-$$HOME/.config}/dev.warp.WarpOss-$${WARP_DATA_PROFILE}" \
				"$${XDG_DATA_HOME:-$$HOME/.local/share}/dev.warp.WarpOss-$${WARP_DATA_PROFILE}" \
				"$${XDG_STATE_HOME:-$$HOME/.local/state}/dev.warp.WarpOss-$${WARP_DATA_PROFILE}" \
				"$${XDG_CACHE_HOME:-$$HOME/.cache}/dev.warp.WarpOss-$${WARP_DATA_PROFILE}" \
			); \
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
		if [ "$$moved" -eq 0 ]; then \
			rmdir "$$trash_dir"; \
			echo "No local Slipstream settings/state paths found."; \
		else \
			echo "Moved local Slipstream settings/state to $$trash_dir"; \
		fi; \
	'

warp-check:
	cargo fmt --check
	cargo check -p warp --bin warp-oss --features gui

# Produces an AppImage under target/dist. `--debug` selects the `dev` cargo
# profile; without it the oss channel builds `release-lto`.
warp-build:
	PATH="$$HOME/.cargo/bin:$$PATH" TERM=xterm-256color ./script/bundle --channel oss --debug

warp-build-optimized:
	PATH="$$HOME/.cargo/bin:$$PATH" TERM=xterm-256color CARGO_PROFILE_RELEASE_SPLIT_DEBUGINFO=unpacked ./script/bundle --channel oss

warp-build-oss: warp-build

# Builds and launches the app directly via cargo, producing no bundle.
warp-run:
	PATH="$$HOME/.cargo/bin:$$PATH" TERM=xterm-256color FEATURES=gui WARP_BIN_NAME=warp-oss WARP_CHANNEL=oss ./script/run
