use super::{Cdp, HarnessError, json};

pub(super) async fn render(
    cdp: &mut Cdp,
    frame_id: &serde_json::Value,
) -> Result<(), HarnessError> {
    let world = cdp
        .call(
            "Page.createIsolatedWorld",
            json!({"frameId":frame_id,"worldName":"ternilo-document-math"}),
        )
        .await?;
    let context = &world["executionContextId"];
    let expression = concat!(
        include_str!("../../assets/document-math.js"),
        r#";
        (() => {
          const formulas = [...document.querySelectorAll('[data-ternilo-math]')];
          for (const [index, element] of formulas.entries()) {
            try {
              element.innerHTML = katex.renderToString(element.textContent, {
                output: 'mathml', displayMode: element.dataset.terniloMath === 'display',
                trust: () => { throw new Error('Links, external resources and HTML commands are not supported in document formulas'); },
                strict: 'error', throwOnError: true, maxExpand: 1000, maxSize: 20
              });
            } catch (error) {
              return { error: `Formula ${index + 1}: ${error.message}` };
            }
          }
          return { rendered: formulas.length };
        })()"#,
    );
    let result = cdp
        .call(
            "Runtime.evaluate",
            json!({"contextId":context,"expression":expression,"returnByValue":true}),
        )
        .await?;
    if let Some(error) = result["result"]["value"]["error"].as_str() {
        return Err(HarnessError::invalid(format!(
            "document math: {error}; correct the source and render again"
        )));
    }
    if result["result"]["value"]["rendered"].as_u64().is_none() {
        return Err(HarnessError::execution(
            "document math renderer did not complete",
        ));
    }
    Ok(())
}
