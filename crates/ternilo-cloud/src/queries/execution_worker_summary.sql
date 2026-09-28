SELECT COUNT(*) AS residents,
    COALESCE(SUM(CASE WHEN phase<>'parked' THEN 1 ELSE 0 END),0) AS nonparked
FROM cloud_run_execution WHERE worker_id=$1 AND phase<>'released'
