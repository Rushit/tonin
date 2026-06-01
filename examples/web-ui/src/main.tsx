import React from "react";
import { createRoot } from "react-dom/client";
import { createPromiseClient } from "@connectrpc/connect";
import { createGrpcWebTransport } from "@connectrpc/connect-web";

// import { Orders } from "./gen/orders/v1/orders_connect"; // after `npm run gen`

const transport = createGrpcWebTransport({
  // In-cluster: web-ui is in the mesh, so this URL is mTLS-encrypted by Linkerd.
  // Locally: dev server proxies /api/* to a port-forwarded orders pod.
  baseUrl: "/api",
});

function App() {
  const [sku, setSku] = React.useState("WIDGET-1");
  const [qty, setQty] = React.useState(1);
  const [result, setResult] = React.useState<string>("");

  async function placeOrder() {
    // const client = createPromiseClient(Orders, transport);
    // const r = await client.placeOrder({ sku, qty, customerId: "demo" });
    // setResult(`order=${r.orderId} accepted=${r.accepted}`);
    setResult(`(stub) would place order sku=${sku} qty=${qty}`);
  }

  return (
    <div style={{ fontFamily: "system-ui", padding: 24 }}>
      <h1>tonin shop</h1>
      <label>SKU <input value={sku} onChange={e => setSku(e.target.value)} /></label>{" "}
      <label>Qty <input type="number" value={qty} onChange={e => setQty(+e.target.value)} /></label>{" "}
      <button onClick={placeOrder}>Place order</button>
      <pre>{result}</pre>
    </div>
  );
}

createRoot(document.getElementById("root")!).render(<App />);
