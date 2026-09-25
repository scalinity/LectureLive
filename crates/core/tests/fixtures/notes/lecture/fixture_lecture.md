# Machine Learning — Week 03 — Optimisation — 2026-09-25

<!-- 10:05:12 -->
## Why optimisation matters
- Training a model means choosing parameters \(\theta\) that make a loss \(L(\theta)\) small on the training data.
- For most models there is no closed-form minimiser, so we search: start somewhere, then repeatedly move to a point with lower loss.
- The lecturer stressed that everything today is about one question: which direction to move, and how far.
- Loss surfaces for neural networks are non-convex: many local minima and saddle points, and flat regions called plateaus.
- In high dimensions saddle points are far more common than bad local minima; most of the difficulty in practice comes from saddles and plateaus, not from getting trapped.

### The gradient
- The gradient \(\nabla L(\theta)\) is the vector of partial derivatives \(\partial L / \partial \theta_i\).
- It points in the direction of steepest increase of the loss; its negative points downhill.
- Its length says how steep the surface is at that point: a small gradient means a flat region, not necessarily a minimum.
- At a minimum the gradient is zero, but a zero gradient can also mean a maximum or a saddle point.

![Slide 1](slides/slide_01_100512.png)
The contour plot of a two-parameter loss with the path of gradient descent zig-zagging down a narrow valley.

## Gradient descent
- The update rule: \(\theta_{t+1} = \theta_t - \eta \nabla L(\theta_t)\).
- \(\eta\) is the learning rate (step size); it is a hyperparameter, not learned.
- One full pass that uses every training example to compute the gradient is called batch gradient descent.
- Each step costs a full pass over the data, which is expensive when the data set has millions of examples.
- **Examinable:** be able to write the update rule and explain every symbol in it.

### Choosing the learning rate
- Too large: the iterates overshoot the minimum, oscillate across the valley, and can diverge (the loss grows without bound).
- Too small: progress is slow and training may stall on plateaus before reaching a good region.
- A good rate is found by trying values on a log scale, e.g. 1, 0.1, 0.01, 0.001, and watching the training loss for the first few hundred steps.
- The lecturer's rule of thumb: pick the largest rate for which the loss still decreases smoothly, then divide by two or three.
- For a quadratic loss \(L(\theta) = \tfrac{1}{2} a \theta^2\) the iteration is \(\theta_{t+1} = (1 - \eta a)\theta_t\); it converges if and only if \(0 < \eta < 2/a\).
- The fastest convergence for that quadratic is at \(\eta = 1/a\), which reaches the minimum in one step.
- In several dimensions the largest curvature (the largest eigenvalue of the Hessian) sets the upper limit on the rate, while the smallest curvature sets how slowly the flat directions converge; their ratio, the condition number, measures how hard the problem is.

### Worked example: one step by hand
- Loss \(L(w) = (w - 3)^2\), starting at \(w_0 = 0\), learning rate \(\eta = 0.1\).
- Gradient: \(L'(w) = 2(w - 3)\), so \(L'(0) = -6\).
- Update: \(w_1 = 0 - 0.1 \times (-6) = 0.6\).
- Next gradient: \(L'(0.6) = -4.8\); update: \(w_2 = 0.6 + 0.48 = 1.08\).
- Each step closes 20% of the remaining distance to the minimum at 3, because \(1 - 2\eta = 0.8\).
- With \(\eta = 1.1\) the factor is \(1 - 2.2 = -1.2\): the iterates alternate sides and grow, so the method diverges.

## Stochastic and mini-batch gradient descent
- Stochastic gradient descent (SGD) estimates the gradient from a single random example per step.
- The estimate is unbiased, its expected value is the true gradient, but it is noisy.
- Mini-batch gradient descent averages the gradient over a small random batch, typically 32 to 512 examples.
- Mini-batches trade noise for cost: larger batches give a less noisy gradient but each step costs more.
- Mini-batches also make good use of vectorised hardware such as GPUs, which process a batch almost as fast as a single example.
- An epoch is one pass through the whole training set; with batch size \(B\) and \(N\) examples there are \(N/B\) steps per epoch.
- The noise in SGD is not only a cost: it helps the iterates escape saddle points and sharp minima, and sharp minima tend to generalise worse.

### Learning-rate schedules
- Because the gradient estimate is noisy, a fixed rate makes the iterates keep bouncing around the minimum.
- Decreasing the rate over time lets them settle: step decay (divide by 10 every few epochs), exponential decay, or cosine annealing.
- Warm-up: start with a small rate for the first few hundred steps and increase it, which stabilises the start of training for large models.
- Classical convergence conditions for SGD: \(\sum_t \eta_t = \infty\) and \(\sum_t \eta_t^2 < \infty\), for example \(\eta_t = \eta_0 / t\).
- The lecturer flagged these two conditions as likely exam material, together with an explanation of why both are needed: the first lets the iterates travel arbitrarily far, the second makes the accumulated noise finite.

![Slide 2](slides/slide_02_101830.png)
A plot of training loss against epochs for a fixed rate, step decay and cosine annealing.

<!-- 10:18:30 -->
## Momentum
- Plain gradient descent zig-zags in narrow valleys: the gradient points mostly across the valley, not along it.
- Momentum keeps a running average of past gradients and moves along that average.
- Update: \(v_{t+1} = \beta v_t + \nabla L(\theta_t)\), then \(\theta_{t+1} = \theta_t - \eta v_{t+1}\).
- \(\beta\) is the momentum coefficient, typically 0.9; the average then covers roughly the last \(1/(1-\beta) = 10\) gradients.
- Across the valley the gradient signs alternate and cancel in the average; along the valley they agree and add up, so the method speeds up in the useful direction.
- Physical picture: a heavy ball rolling down the surface, which builds up speed and is not deflected by every small bump.
- With a constant gradient \(g\) the velocity approaches \(g/(1-\beta)\), so momentum effectively multiplies the learning rate by up to ten.

### Nesterov momentum
- Nesterov's variant evaluates the gradient at the look-ahead point \(\theta_t - \eta \beta v_t\) instead of at \(\theta_t\).
- Update: \(v_{t+1} = \beta v_t + \nabla L(\theta_t - \eta \beta v_t)\), \(\theta_{t+1} = \theta_t - \eta v_{t+1}\).
- It corrects the step before overshooting, which reduces oscillation; for convex problems it has a provably faster convergence rate.
- In practice the difference from ordinary momentum is small but consistent.

## Adaptive methods
- A single global learning rate is a poor fit when different parameters see gradients of very different sizes, for example rare and frequent words in a text model.
- Adaptive methods keep a separate effective rate per parameter, scaled by the history of that parameter's gradients.

### AdaGrad
- Accumulates squared gradients: \(G_t = G_{t-1} + g_t^2\) (element-wise).
- Update: \(\theta_{t+1} = \theta_t - \eta\, g_t / (\sqrt{G_t} + \epsilon)\).
- Parameters with large past gradients get smaller steps; rarely updated parameters keep large steps.
- Weakness: \(G_t\) only grows, so the effective rate shrinks towards zero and training can stop too early.

### RMSProp
- Replaces the sum with an exponential moving average: \(s_t = \rho s_{t-1} + (1-\rho) g_t^2\), with \(\rho\) around 0.9.
- Update: \(\theta_{t+1} = \theta_t - \eta\, g_t / (\sqrt{s_t} + \epsilon)\).
- Because old gradients are forgotten, the effective rate does not decay to zero.

![Slide 3](slides/slide_03_103245.png)
A table comparing SGD, momentum, AdaGrad, RMSProp and Adam: state kept per parameter, typical hyperparameters, and when each is used.

<!-- 10:32:45 -->
### Adam
- Adam combines momentum (a moving average of gradients) with RMSProp (a moving average of squared gradients).
- First moment: \(m_t = \beta_1 m_{t-1} + (1-\beta_1) g_t\); second moment: \(v_t = \beta_2 v_{t-1} + (1-\beta_2) g_t^2\).
- Bias correction, because both averages start at zero: \(\hat m_t = m_t / (1-\beta_1^t)\), \(\hat v_t = v_t / (1-\beta_2^t)\).
- Update: \(\theta_{t+1} = \theta_t - \eta\, \hat m_t / (\sqrt{\hat v_t} + \epsilon)\).
- Default hyperparameters: \(\beta_1 = 0.9\), \(\beta_2 = 0.999\), \(\epsilon = 10^{-8}\), \(\eta = 10^{-3}\).
- **Examinable:** explain why bias correction is needed; without it the first steps are far too small, because \(m_1 = (1-\beta_1) g_1 = 0.1\, g_1\).
- Worked check at step 1: \(\hat m_1 = g_1\) and \(\hat v_1 = g_1^2\), so the first step has size about \(\eta\) in every coordinate, whatever the gradient's scale.
- Adam is the default choice for most deep-learning problems because it works well with little tuning.

### AdamW and weight decay
- L2 regularisation adds \(\tfrac{\lambda}{2}\|\theta\|^2\) to the loss, which adds \(\lambda\theta\) to the gradient.
- In Adam that extra term is divided by \(\sqrt{\hat v_t}\), so parameters with large gradients are regularised less than intended.
- AdamW decouples the decay: it applies \(\theta \leftarrow \theta - \eta\lambda\theta\) directly, outside the adaptive scaling.
- The lecturer recommended AdamW over Adam with L2 for anything with weight decay.

## Practical advice from the lecture
- Always normalise the inputs: features on very different scales make the loss surface badly conditioned and slow every method down.
- Monitor both training and validation loss; if the training loss is not falling, the problem is the optimiser or the learning rate, not overfitting.
- If the loss becomes NaN, the learning rate is almost always too large, or there is a division by zero somewhere in the model.
- Gradient clipping caps the norm of the gradient, for example at 1.0, which prevents a single bad batch from throwing the parameters far away; it is standard for recurrent networks.
- Start with Adam or AdamW at \(10^{-3}\); try SGD with momentum 0.9 and a decaying rate if you need the best final accuracy on vision problems.
- Batch size and learning rate interact: when the batch size doubles, a common heuristic is to double the learning rate as well (the linear scaling rule), with warm-up.
- Save checkpoints regularly so that a diverging run can be restarted from the last good point with a smaller rate.

## Second-order methods (brief)
- Newton's method uses the Hessian \(H\): \(\theta_{t+1} = \theta_t - H^{-1} \nabla L(\theta_t)\).
- It converges in very few steps near a minimum, and on a quadratic in a single step, because it rescales every direction by its curvature.
- For a model with \(n\) parameters the Hessian has \(n^2\) entries; with millions of parameters it can neither be stored nor inverted.
- Quasi-Newton methods such as L-BFGS build a low-rank approximation of the inverse Hessian from recent gradients and are used for smaller, full-batch problems.
- Adaptive methods like Adam can be seen as a cheap diagonal approximation of curvature information.

## Summary of the lecture
- Gradient descent moves against the gradient with a step size set by the learning rate; the rate is the most important hyperparameter.
- Stochastic and mini-batch versions make each step cheap at the price of noise, which a decaying schedule controls.
- Momentum and Nesterov momentum smooth the path and speed up progress along narrow valleys.
- AdaGrad, RMSProp and Adam adapt the step per parameter; Adam with bias correction is the usual default, AdamW when weight decay is used.
- Next week: backpropagation, how the gradient itself is computed efficiently for a network with many layers.

## Questions raised in class
- Does the noise in SGD always help generalisation? The lecturer said it helps on average but not for every problem, and that very large batches often need extra regularisation to match small-batch results.
- Why not always use Newton's method? Because of the cost of the Hessian, and because near saddle points it can move towards the saddle instead of away from it.
- How is \(\epsilon\) chosen in Adam? It only prevents division by zero; its value rarely matters, although some models are trained with \(\epsilon = 10^{-6}\) for stability in low precision.
