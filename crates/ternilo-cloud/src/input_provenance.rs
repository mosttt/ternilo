use sqlx::{Row, any::AnyRow};
use ternilo_protocol::{HarnessError, InputProvenance, SessionEventKind, UserMessageSource};
use ternilo_storage::{Json, Transaction, database_error};

use crate::StartedRun;

/// Compare executor-reported authors with the input accepted by the control plane.
pub(crate) async fn validate_user_message_in(
    transaction: &mut Transaction,
    run: &StartedRun,
    canonical_run: &AnyRow,
    event: &SessionEventKind,
) -> Result<(), HarnessError> {
    let SessionEventKind::UserMessage {
        provenance, source, ..
    } = event
    else {
        return Ok(());
    };
    let expected = match source {
        Some(UserMessageSource::Submission { submission_id, .. }) => {
            let row = sqlx::query(
                "SELECT input_provenance FROM cloud_session_submissions
                WHERE tenant_id=$1 AND session_id=$2 AND submission_id=$3
                  AND (run_id=$4 OR batch_run_id=$4
                    OR (steering_target_run_id=$4 AND steering_target_writer_fencing_token=$5))",
            )
            .bind(run.claim.tenant_id.as_str())
            .bind(run.claim.session_id.as_str())
            .bind(submission_id.as_str())
            .bind(run.claim.run_id.as_str())
            .bind(crate::store::to_i64(
                run.fencing_token,
                "input writer fencing token",
            )?)
            .fetch_optional(&mut **transaction)
            .await
            .map_err(database_error)?
            .ok_or_else(unverified_author)?;
            stored_provenance(&row)?
        }
        _ => stored_provenance(canonical_run)?,
    };
    if provenance != &expected {
        return Err(unverified_author());
    }
    if let Some(provenance) = provenance {
        provenance.validate()?;
        if let Some(UserMessageSource::Submission { submission_id, .. }) = source
            && submission_id != &provenance.input_id
        {
            return Err(unverified_author());
        }
    }
    Ok(())
}

pub(crate) fn stored_provenance(row: &AnyRow) -> Result<Option<InputProvenance>, HarnessError> {
    let provenance = row
        .try_get::<Option<Json<InputProvenance>>, _>("input_provenance")
        .map_err(database_error)?
        .map(|value| value.0);
    if let Some(provenance) = &provenance {
        provenance.validate()?;
    }
    Ok(provenance)
}

fn unverified_author() -> HarnessError {
    HarnessError::policy("input author does not match the accepted cloud input")
}
