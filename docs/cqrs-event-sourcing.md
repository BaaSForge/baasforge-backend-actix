### Communications between units

```mermaid
---
config:
  layout: dagre
  theme: neo-dark
---
flowchart TB
 subgraph External["External"]
        CLIENT["Client / API Gateway"]
  end
 subgraph Units["Units"]
        CMD["Command"]
        AGG["Aggregator"]
        SNAP["Snapshotter"]
        PROJ["Projection"]
        QUERY["Query"]
        SAGA["Saga"]
  end
 subgraph NATS["NATS (Event Store + Message Bus)"]
        STREAM["JetStream – Event Streams"]
        REQ["Request/Reply (sync)"]
        PUB["Pub/Sub (async events)"]
  end
    CLIENT -- "1. NATS Request (sync)\ncmd.order.create" --> CMD
    CMD -- "2. NATS Request (sync)\nagg.order.load\n{aggregateId}" --> AGG
    AGG -- "3. NATS Request (sync)\nsnap.order.get\n{aggregateId}" --> SNAP
    SNAP -- "4. Returns latest snapshot\n{version, state}" --> AGG
    AGG -- "5. NATS JetStream Fetch\nevents.order.{aggregateId}\nfrom snapshot version+1" --> STREAM
    STREAM -- "6. Returns events\nsince snapshot" --> AGG
    AGG -- "7. Returns hydrated\naggregate state\n{version, state}" --> CMD
    CMD -- "8. Business logic validates\nthen NATS JetStream Publish\nevents.order.OrderCreated\n(expected_version for OCC)" --> STREAM
    CMD -- "9. NATS Reply (sync)\n{success, aggregateId}" --> CLIENT
    STREAM -- "10. JetStream Push/Pull\nConsumer (durable)\nevents.order.>" --> PROJ
    PROJ -- "11. Updates read model\n(KV / DB / NATS KV Store)" --> PROJ
    STREAM -- "12. JetStream Push/Pull\nConsumer (durable)\nevents.order.>" --> SAGA
    SAGA -- "13. NATS Request (sync)\ncmd.payment.charge\n(compensating actions too)" --> CMD
    STREAM -- "14. JetStream Consumer\nevery N events or\non schedule" --> SNAP
    SNAP -- "15. NATS Request (sync)\nagg.order.load\n(full rebuild)" --> AGG
    SNAP -- "16. Stores snapshot\nNATS KV or Object Store\n{aggregateId, version, state}" --> SNAP
    CLIENT -- "17. NATS Request (sync)\nquery.order.getById" --> QUERY
    QUERY -- "18. Reads from\nprojection's read model\n(NATS KV / external DB)" --> PROJ
    QUERY -- "19. NATS Reply (sync)\n{order data}" --> CLIENT

     CLIENT:::ext
     CMD:::unit
     AGG:::unit
     SNAP:::unit
     PROJ:::unit
     QUERY:::unit
     SAGA:::unit
     STREAM:::nats
     REQ:::nats
     PUB:::nats
    classDef unit fill:#1a1a2e,stroke:#e94560,color:#fff,stroke-width:2px
    classDef nats fill:#0f3460,stroke:#00d2ff,color:#fff,stroke-width:2px
    classDef ext fill:#16213e,stroke:#ffd700,color:#fff,stroke-width:2px
```