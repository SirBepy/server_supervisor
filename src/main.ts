import "@phosphor-icons/web/regular";
import "./styles/base.css";
import { mountDashboard } from "./views/dashboard/dashboard";

const app = document.getElementById("app")!;
mountDashboard(app);
