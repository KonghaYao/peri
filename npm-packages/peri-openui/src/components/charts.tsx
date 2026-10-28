import { BarChart, LineChart, PieChart, type ChartDataPoint, type ChartSeries, type ChartTone } from "../canvas";
import { ChartWarning, chartDataMismatch, type OpenUIRenderProps } from "./shared";

export interface BarChartShape {
  categories: string[];
  series: ChartSeries[];
  height?: number;
  stacked?: boolean;
  horizontal?: boolean;
  normalized?: boolean;
  valuePrefix?: string;
  valueSuffix?: string;
  showValues?: boolean;
}

export function BarChartView({ props }: OpenUIRenderProps<BarChartShape>) {
  if (chartDataMismatch(props.series, props.categories)) return <ChartWarning />;
  return (
    <BarChart
      categories={props.categories}
      series={props.series}
      height={props.height}
      stacked={props.stacked}
      horizontal={props.horizontal}
      normalized={props.normalized}
      valuePrefix={props.valuePrefix}
      valueSuffix={props.valueSuffix}
      showValues={props.showValues}
    />
  );
}

export interface LineChartShape {
  categories: string[];
  series: ChartSeries[];
  height?: number;
  fill?: boolean;
  valuePrefix?: string;
  valueSuffix?: string;
  showValues?: boolean;
}

export function LineChartView({ props }: OpenUIRenderProps<LineChartShape>) {
  if (chartDataMismatch(props.series, props.categories)) return <ChartWarning />;
  return (
    <LineChart
      categories={props.categories}
      series={props.series}
      height={props.height}
      fill={props.fill}
      valuePrefix={props.valuePrefix}
      valueSuffix={props.valueSuffix}
      showValues={props.showValues}
    />
  );
}

export interface PieChartShape {
  data: Array<ChartDataPoint & { tone?: ChartTone }>;
  size?: number;
  donut?: boolean;
}

export function PieChartView({ props }: OpenUIRenderProps<PieChartShape>) {
  const data = props.data.filter((entry) => Number.isFinite(entry.value));
  if (data.length === 0) {
    return <ChartWarning message="没有有效的数值，已省略该图。" />;
  }
  return <PieChart data={data} size={props.size} donut={props.donut} />;
}
