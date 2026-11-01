use serde_json::{Value, json};
use strum_macros::{EnumDiscriminants, EnumIter};

use warp_core::user_preferences::GetUserPreferences;
use warpui::{AppContext, Entity, ModelContext, SingletonEntity};

use crate::pricing::{PricingInfoModel, PricingInfoModelEvent};

const AGENT_DISMISSED_KEY: &str = "pricing_promotion_agent_dismissed";
const TERMINAL_DISMISSED_KEY: &str = "pricing_promotion_terminal_dismissed";
const DISMISSED_VALUE: &str = "true";

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum PricingPromotionSurface {
    AgentMessageBar,
    TerminalMessageBar,
}

impl PricingPromotionSurface {
    fn as_str(self) -> &'static str {
        match self {
            Self::AgentMessageBar => "agent_message_bar",
            Self::TerminalMessageBar => "terminal_message_bar",
        }
    }

    fn dismissal_key(self) -> &'static str {
        match self {
            Self::AgentMessageBar => AGENT_DISMISSED_KEY,
            Self::TerminalMessageBar => TERMINAL_DISMISSED_KEY,
        }
    }
}

#[derive(Clone, Debug)]
pub enum PricingPromotionStateEvent {
    Updated,
}

pub struct PricingPromotionState {
    agent_dismissed: bool,
    terminal_dismissed: bool,
}

impl PricingPromotionState {
    pub fn new(ctx: &mut ModelContext<Self>) -> Self {
        ctx.subscribe_to_model(&PricingInfoModel::handle(ctx), |_, _, event, ctx| {
            if matches!(event, PricingInfoModelEvent::PricingInfoUpdated) {
                ctx.emit(PricingPromotionStateEvent::Updated);
                ctx.notify();
            }
        });

        Self {
            agent_dismissed: Self::read_dismissed(AGENT_DISMISSED_KEY, ctx),
            terminal_dismissed: Self::read_dismissed(TERMINAL_DISMISSED_KEY, ctx),
        }
    }

    pub fn visible_message(
        &self,
        surface: PricingPromotionSurface,
        app: &AppContext,
    ) -> Option<String> {
        if self.is_dismissed(surface) {
            return None;
        }
        PricingInfoModel::as_ref(app)
            .promotion_message()
            .map(str::to_owned)
    }

    pub fn dismiss(&mut self, surface: PricingPromotionSurface, ctx: &mut ModelContext<Self>) {
        match surface {
            PricingPromotionSurface::AgentMessageBar => self.agent_dismissed = true,
            PricingPromotionSurface::TerminalMessageBar => self.terminal_dismissed = true,
        }
        if let Err(error) = ctx
            .private_user_preferences()
            .write_value(surface.dismissal_key(), DISMISSED_VALUE.to_string())
        {
            log::warn!("Failed to persist pricing promotion dismissal: {error:#}");
        }

        ctx.emit(PricingPromotionStateEvent::Updated);
        ctx.notify();
    }

    fn is_dismissed(&self, surface: PricingPromotionSurface) -> bool {
        match surface {
            PricingPromotionSurface::AgentMessageBar => self.agent_dismissed,
            PricingPromotionSurface::TerminalMessageBar => self.terminal_dismissed,
        }
    }

    fn read_dismissed(key: &str, ctx: &AppContext) -> bool {
        ctx.private_user_preferences()
            .read_value(key)
            .unwrap_or_default()
            .is_some_and(|value| value == DISMISSED_VALUE)
    }
}

impl Entity for PricingPromotionState {
    type Event = PricingPromotionStateEvent;
}

impl SingletonEntity for PricingPromotionState {}

#[cfg(test)]
#[path = "pricing_promotion_tests.rs"]
mod tests;
