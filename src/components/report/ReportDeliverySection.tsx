import type { MissionDeliverySnapshot } from "../../ipc/commands";

interface ReportDeliverySectionProps {
  delivery?: MissionDeliverySnapshot | null;
}

export function ReportDeliverySection({ delivery }: ReportDeliverySectionProps) {
  if (!delivery) {
    return <p>No delivery snapshot is attached to this report.</p>;
  }

  const [primary, ...supporting] = delivery.items;

  return (
    <div>
      <p>{delivery.overview.summary}</p>

      <h4>Primary Deliverable</h4>
      {primary ? <DeliveryItemView item={primary} /> : <p>No primary deliverable was identified.</p>}

      {supporting.length ? (
        <>
          <h4>Supporting Deliverables</h4>
          <ul>
            {supporting.map((item) => (
              <li key={item.id}>
                <DeliveryItemView item={item} />
              </li>
            ))}
          </ul>
        </>
      ) : null}

      {delivery.how_to_use.length ? (
        <>
          <h4>How to use</h4>
          <ul>
            {delivery.how_to_use.map((step) => (
              <li key={`${step.title}-${step.detail}`}>
                <strong>{step.title}</strong>: {step.detail}
              </li>
            ))}
          </ul>
        </>
      ) : null}

      {delivery.validation.length ? (
        <>
          <h4>Validation</h4>
          <ul>
            {delivery.validation.map((entry, index) => (
              <li key={`${entry.status}-${entry.summary}-${index}`}>
                <strong>{entry.status}</strong>: {entry.summary}
                {entry.command ? <code>{entry.command}</code> : null}
              </li>
            ))}
          </ul>
        </>
      ) : null}

      {delivery.caveats.length ? (
        <>
          <h4>Caveats</h4>
          <ul>
            {delivery.caveats.map((caveat) => (
              <li key={caveat}>{caveat}</li>
            ))}
          </ul>
        </>
      ) : null}
    </div>
  );
}

function DeliveryItemView({ item }: { item: MissionDeliverySnapshot["items"][number] }) {
  return (
    <div>
      <strong>{item.title}</strong> ({item.source}, confidence: {item.confidence})
      {item.summary ? <p>{item.summary}</p> : null}
      {item.file_paths.map((path) => (
        <code key={path}>{path}</code>
      ))}
    </div>
  );
}
