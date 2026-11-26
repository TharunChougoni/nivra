import type React from "react";
import { Backtest } from "./BacktestPage";
import { Health } from "./HealthPage";
import Instruments from "./Instruments";
import Lanes from "./Lanes";
import Overview from "./Overview";
import { Portfolio } from "./PortfolioPage";
import { Scanner } from "./ScannerPage";
import { Sessions } from "./SessionsPage";
import { Strategies } from "./StrategiesPage";

export {
  Backtest,
  Health,
  Instruments,
  Lanes,
  Overview,
  Portfolio,
  Scanner,
  Sessions,
  Strategies,
};

export const PAGES: Record<string, React.ComponentType> = {
  overview: Overview,
  instruments: Instruments,
  portfolio: Portfolio,
  scanner: Scanner,
  sessions: Sessions,
  strategies: Strategies,
  backtest: Backtest,
  health: Health,
  lanes: Lanes,
};
